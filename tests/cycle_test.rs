use terragrunt_dag::{DependencyEdge, EdgeType, analyze_cycles, detect_cycles, report_cycles};

#[test]
fn test_simple_cycle_example() {
    let edges = vec![
        DependencyEdge {
            from: "/live/prod/vpc".to_string(),
            to: "/live/prod/app".to_string(),
            edge_type: EdgeType::Dependency,
        },
        DependencyEdge {
            from: "/live/prod/app".to_string(),
            to: "/live/prod/vpc".to_string(),
            edge_type: EdgeType::Include,
        },
    ];

    let result = detect_cycles(&edges);

    assert_eq!(result.cycles.len(), 1);
    assert_eq!(result.in_cycle.len(), 2);

    // Test report output
    let mut output = Vec::new();
    report_cycles(&result, &mut output).unwrap();
    let output_str = String::from_utf8(output).unwrap();
    assert!(output_str.contains("WARNING: Dependency cycles detected"));
}

#[test]
fn test_mixed_dag_and_cycle_example() {
    let edges = vec![
        DependencyEdge {
            from: "/live/common/root".to_string(),
            to: "/live/prod/vpc".to_string(),
            edge_type: EdgeType::Include,
        },
        DependencyEdge {
            from: "/live/prod/vpc".to_string(),
            to: "/live/prod/sg".to_string(),
            edge_type: EdgeType::Dependency,
        },
        DependencyEdge {
            from: "/live/prod/sg".to_string(),
            to: "/live/prod/app".to_string(),
            edge_type: EdgeType::Dependency,
        },
        DependencyEdge {
            from: "/live/prod/app".to_string(),
            to: "/live/prod/vpc".to_string(),
            edge_type: EdgeType::ReadConfig,
        },
    ];

    let result = detect_cycles(&edges);

    // Should have one cycle (vpc -> sg -> app -> vpc)
    assert_eq!(result.cycles.len(), 1);
    assert_eq!(result.in_cycle.len(), 3);

    // root should be in valid topological order (not in cycle)
    assert!(result.ordered.contains(&"/live/common/root".to_string()));
    assert!(!result.in_cycle.contains("/live/common/root"));

    // Test analyze output
    let mut output = Vec::new();
    analyze_cycles(&result, &edges, &mut output).unwrap();
    let output_str = String::from_utf8(output).unwrap();
    assert!(output_str.contains("Cycle Analysis"));
    assert!(output_str.contains("SUGGESTED BREAK"));
}

#[test]
fn test_no_cycle_with_analyze() {
    let edges = vec![
        DependencyEdge {
            from: "/live/common/root".to_string(),
            to: "/live/prod/vpc".to_string(),
            edge_type: EdgeType::Include,
        },
        DependencyEdge {
            from: "/live/prod/vpc".to_string(),
            to: "/live/prod/app".to_string(),
            edge_type: EdgeType::Dependency,
        },
    ];

    let result = detect_cycles(&edges);

    assert_eq!(result.cycles.len(), 0);
    assert_eq!(result.ordered.len(), 3);

    let mut output = Vec::new();
    analyze_cycles(&result, &edges, &mut output).unwrap();
    let output_str = String::from_utf8(output).unwrap();
    assert!(output_str.contains("✓ No cycles detected"));
}

#[test]
fn test_realistic_terragrunt_scenario() {
    // Simulate a realistic scenario where:
    // - vpc depends on nothing (just projects)
    // - sg depends on vpc
    // - app depends on sg and vpc
    // All have includes to common/root.hcl but that's a watch file (edge FROM project TO file)

    let edges = vec![
        // Project to project dependencies
        DependencyEdge {
            from: "/live/prod/sg/terragrunt.hcl".to_string(),
            to: "/live/prod/vpc/terragrunt.hcl".to_string(),
            edge_type: EdgeType::Dependency,
        },
        DependencyEdge {
            from: "/live/prod/app/terragrunt.hcl".to_string(),
            to: "/live/prod/sg/terragrunt.hcl".to_string(),
            edge_type: EdgeType::Dependency,
        },
        DependencyEdge {
            from: "/live/prod/app/terragrunt.hcl".to_string(),
            to: "/live/prod/vpc/terragrunt.hcl".to_string(),
            edge_type: EdgeType::Dependency,
        },
    ];

    let result = detect_cycles(&edges);

    // Should have no cycles
    assert_eq!(result.cycles.len(), 0);
    assert_eq!(result.in_cycle.len(), 0);

    // All 3 projects should be in topological order
    assert_eq!(result.ordered.len(), 3);

    // Verify order: vpc comes before sg, sg comes before app
    let vpc_pos = result.ordered.iter().position(|n| n == "/live/prod/vpc/terragrunt.hcl").unwrap();
    let sg_pos = result.ordered.iter().position(|n| n == "/live/prod/sg/terragrunt.hcl").unwrap();
    let app_pos = result.ordered.iter().position(|n| n == "/live/prod/app/terragrunt.hcl").unwrap();

    assert!(vpc_pos < sg_pos);
    assert!(sg_pos < app_pos);
}
