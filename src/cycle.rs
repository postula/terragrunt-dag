//! Cycle detection and breaking for project dependency graphs
//!
//! Implements Kahn's algorithm for topological sorting and cycle detection,
//! with cycle extraction using DFS.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;

/// A dependency edge in the project graph
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyEdge {
    pub from: String,
    pub to: String,
    pub edge_type: EdgeType,
}

/// Edge types with weights for cycle breaking (lower = break first)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeType {
    /// dependency block (config_path)
    Dependency,
    /// include block
    Include,
    /// terraform.source (local)
    TerraformSource,
    /// read_terragrunt_config()
    ReadConfig,
    /// file(), sops_decrypt_file()
    FileRead,
}

impl EdgeType {
    /// Weight for cycle breaking score (lower = safer to break)
    pub fn weight(&self) -> i32 {
        match self {
            EdgeType::ReadConfig => 2,
            EdgeType::Include => 3,
            EdgeType::FileRead => 4,
            EdgeType::TerraformSource => 5,
            EdgeType::Dependency => 8,
        }
    }
}

/// A cycle in the dependency graph
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cycle {
    /// Projects forming the cycle (last connects back to first)
    pub nodes: Vec<String>,
    /// Edges in the cycle
    pub edges: Vec<DependencyEdge>,
}

/// Result of cycle detection
#[derive(Debug)]
pub struct CycleDetectionResult {
    /// All detected cycles
    pub cycles: Vec<Cycle>,
    /// Projects in valid topological order (only those NOT in cycles)
    pub ordered: Vec<String>,
    /// Projects that are part of cycles (couldn't be ordered)
    pub in_cycle: HashSet<String>,
}

/// Detect cycles using Kahn's algorithm
///
/// Uses topological sort to identify cycles. Nodes remaining after
/// the sort are part of cycles.
pub fn detect_cycles(edges: &[DependencyEdge]) -> CycleDetectionResult {
    // Build adjacency list and in-degree count
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    let mut in_degree: HashMap<String, usize> = HashMap::new();
    let mut all_nodes: HashSet<String> = HashSet::new();

    for edge in edges {
        all_nodes.insert(edge.from.clone());
        all_nodes.insert(edge.to.clone());

        graph.entry(edge.from.clone()).or_default().push(edge.to.clone());

        *in_degree.entry(edge.to.clone()).or_insert(0) += 1;
        in_degree.entry(edge.from.clone()).or_insert(0);
    }

    // Kahn's algorithm: start with nodes that have no incoming edges
    let mut queue: VecDeque<String> =
        in_degree.iter().filter(|(_, deg)| **deg == 0).map(|(node, _)| node.clone()).collect();

    let mut ordered = Vec::new();
    let mut in_degree_copy = in_degree.clone();

    while let Some(node) = queue.pop_front() {
        ordered.push(node.clone());

        if let Some(neighbors) = graph.get(&node) {
            for neighbor in neighbors {
                if let Some(deg) = in_degree_copy.get_mut(neighbor) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(neighbor.clone());
                    }
                }
            }
        }
    }

    // Nodes not in ordered list are part of cycles
    let in_cycle: HashSet<String> = all_nodes.into_iter().filter(|node| !ordered.contains(node)).collect();

    // Extract actual cycles from the unordered nodes
    let cycles = if in_cycle.is_empty() {
        Vec::new()
    } else {
        find_cycles(&in_cycle, edges)
    };

    // Reverse the order to get dependencies-first ordering
    // Kahn's algorithm produces a valid topological sort, but we want
    // dependencies (sources) before dependents (sinks)
    ordered.reverse();

    CycleDetectionResult {
        cycles,
        ordered,
        in_cycle,
    }
}

/// Extract actual cycle paths from unordered nodes using DFS
fn find_cycles(unordered: &HashSet<String>, edges: &[DependencyEdge]) -> Vec<Cycle> {
    // Build adjacency list for cycle nodes only
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    let cycle_edges: Vec<DependencyEdge> =
        edges.iter().filter(|e| unordered.contains(&e.from) && unordered.contains(&e.to)).cloned().collect();

    for edge in &cycle_edges {
        graph.entry(edge.from.clone()).or_default().push(edge.to.clone());
    }

    let mut cycles = Vec::new();
    let mut visited = HashSet::new();

    for start_node in unordered {
        if visited.contains(start_node) {
            continue;
        }

        // DFS to find a cycle starting from this node
        let mut path = Vec::new();
        let mut path_set = HashSet::new();

        if let Some(cycle_path) = dfs_find_cycle(start_node, &graph, &mut path, &mut path_set, &mut HashSet::new()) {
            // Mark all nodes in this cycle as visited
            for node in &cycle_path {
                visited.insert(node.clone());
            }

            // Build edges for this cycle
            let mut cycle_edges_list = Vec::new();
            for i in 0..cycle_path.len() {
                let from = &cycle_path[i];
                let to = &cycle_path[(i + 1) % cycle_path.len()];

                // Find the edge
                if let Some(edge) = cycle_edges.iter().find(|e| e.from == *from && e.to == *to) {
                    cycle_edges_list.push(edge.clone());
                }
            }

            cycles.push(Cycle {
                nodes: cycle_path,
                edges: cycle_edges_list,
            });
        }
    }

    cycles
}

/// DFS to find a cycle from current node
fn dfs_find_cycle(
    node: &str,
    graph: &HashMap<String, Vec<String>>,
    path: &mut Vec<String>,
    path_set: &mut HashSet<String>,
    visited: &mut HashSet<String>,
) -> Option<Vec<String>> {
    if path_set.contains(node) {
        // Found a cycle - extract it from the path
        let cycle_start = path.iter().position(|n| n == node).unwrap();
        return Some(path[cycle_start..].to_vec());
    }

    if visited.contains(node) {
        return None;
    }

    path.push(node.to_string());
    path_set.insert(node.to_string());

    if let Some(neighbors) = graph.get(node) {
        for neighbor in neighbors {
            if let Some(cycle) = dfs_find_cycle(neighbor, graph, path, path_set, visited) {
                return Some(cycle);
            }
        }
    }

    path.pop();
    path_set.remove(node);
    visited.insert(node.to_string());

    None
}

/// Report cycles to stderr in human-readable format
pub fn report_cycles(result: &CycleDetectionResult, stderr: &mut dyn Write) -> std::io::Result<()> {
    if result.cycles.is_empty() {
        return Ok(());
    }

    writeln!(
        stderr,
        "⚠️  WARNING: Dependency cycles detected ({} cycle{} found)\n",
        result.cycles.len(),
        if result.cycles.len() == 1 {
            ""
        } else {
            "s"
        }
    )?;

    for (idx, cycle) in result.cycles.iter().enumerate() {
        writeln!(stderr, "Cycle #{}:", idx + 1)?;
        for node in &cycle.nodes {
            writeln!(stderr, "  → {}", node)?;
        }
        if let Some(first) = cycle.nodes.first() {
            writeln!(stderr, "  ↩ back to {}\n", first)?;
        }
    }

    Ok(())
}

/// Detailed cycle analysis (for cycles subcommand)
pub fn analyze_cycles(
    result: &CycleDetectionResult,
    edges: &[DependencyEdge],
    stderr: &mut dyn Write,
) -> std::io::Result<()> {
    if result.cycles.is_empty() {
        writeln!(stderr, "✓ No cycles detected")?;
        return Ok(());
    }

    writeln!(stderr, "🔍 Cycle Analysis\n")?;
    writeln!(stderr, "Found {} cycle(s) affecting {} project(s)\n", result.cycles.len(), result.in_cycle.len())?;

    // Calculate node degrees for scoring
    let mut in_degree: HashMap<String, usize> = HashMap::new();
    let mut out_degree: HashMap<String, usize> = HashMap::new();

    for edge in edges {
        *in_degree.entry(edge.to.clone()).or_insert(0) += 1;
        *out_degree.entry(edge.from.clone()).or_insert(0) += 1;
    }

    for (idx, cycle) in result.cycles.iter().enumerate() {
        writeln!(stderr, "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━")?;
        writeln!(stderr, "Cycle #{} ({} projects)\n", idx + 1, cycle.nodes.len())?;

        writeln!(stderr, "Path:")?;
        for (i, node) in cycle.nodes.iter().enumerate() {
            let in_deg = in_degree.get(node).unwrap_or(&0);
            let out_deg = out_degree.get(node).unwrap_or(&0);
            writeln!(stderr, "  {}. {} (in-degree: {}, out-degree: {})", i + 1, node, in_deg, out_deg)?;
        }

        // Score edges for breaking
        let mut scored_edges: Vec<(usize, i32, &DependencyEdge)> = cycle
            .edges
            .iter()
            .enumerate()
            .map(|(i, edge)| {
                let type_weight = edge.edge_type.weight();
                let target_in_deg = *in_degree.get(&edge.to).unwrap_or(&0);
                let source_out_deg = *out_degree.get(&edge.from).unwrap_or(&0);

                // Simple scoring: lower = break first
                let score = (type_weight * 10) - (target_in_deg.min(10) as i32 * 5)
                    + if source_out_deg <= 2 {
                        30
                    } else if source_out_deg <= 5 {
                        15
                    } else {
                        0
                    };

                (i, score, edge)
            })
            .collect();

        scored_edges.sort_by_key(|(_, score, _)| *score);

        writeln!(stderr, "\nEdges (sorted by break priority):")?;
        for (idx, (_, score, edge)) in scored_edges.iter().enumerate() {
            let suggestion = if idx == 0 {
                " → SUGGESTED BREAK"
            } else {
                ""
            };
            writeln!(stderr, "  {} → {} | {:?} | score: {}{}", edge.from, edge.to, edge.edge_type, score, suggestion)?;
        }
        writeln!(stderr)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_cycles_returns_topological_order() {
        let edges = vec![
            DependencyEdge {
                from: "a".to_string(),
                to: "b".to_string(),
                edge_type: EdgeType::Dependency,
            },
            DependencyEdge {
                from: "b".to_string(),
                to: "c".to_string(),
                edge_type: EdgeType::Dependency,
            },
        ];

        let result = detect_cycles(&edges);

        assert_eq!(result.cycles.len(), 0);
        assert_eq!(result.in_cycle.len(), 0);
        assert_eq!(result.ordered.len(), 3);

        // Verify topological order: dependencies first
        // a→b means "a depends on b", so b must come before a
        // b→c means "b depends on c", so c must come before b
        // Result order should be: c, b, a
        let a_pos = result.ordered.iter().position(|n| n == "a").unwrap();
        let b_pos = result.ordered.iter().position(|n| n == "b").unwrap();
        let c_pos = result.ordered.iter().position(|n| n == "c").unwrap();
        assert!(c_pos < b_pos);
        assert!(b_pos < a_pos);
    }

    #[test]
    fn test_simple_two_node_cycle() {
        let edges = vec![
            DependencyEdge {
                from: "a".to_string(),
                to: "b".to_string(),
                edge_type: EdgeType::Dependency,
            },
            DependencyEdge {
                from: "b".to_string(),
                to: "a".to_string(),
                edge_type: EdgeType::Include,
            },
        ];

        let result = detect_cycles(&edges);

        assert_eq!(result.cycles.len(), 1);
        assert_eq!(result.in_cycle.len(), 2);
        assert!(result.in_cycle.contains("a"));
        assert!(result.in_cycle.contains("b"));
        assert_eq!(result.ordered.len(), 0);

        let cycle = &result.cycles[0];
        assert_eq!(cycle.nodes.len(), 2);
        assert_eq!(cycle.edges.len(), 2);
    }

    #[test]
    fn test_three_node_cycle() {
        let edges = vec![
            DependencyEdge {
                from: "a".to_string(),
                to: "b".to_string(),
                edge_type: EdgeType::Dependency,
            },
            DependencyEdge {
                from: "b".to_string(),
                to: "c".to_string(),
                edge_type: EdgeType::Include,
            },
            DependencyEdge {
                from: "c".to_string(),
                to: "a".to_string(),
                edge_type: EdgeType::ReadConfig,
            },
        ];

        let result = detect_cycles(&edges);

        assert_eq!(result.cycles.len(), 1);
        assert_eq!(result.in_cycle.len(), 3);
        assert_eq!(result.ordered.len(), 0);

        let cycle = &result.cycles[0];
        assert_eq!(cycle.nodes.len(), 3);
        assert_eq!(cycle.edges.len(), 3);
    }

    #[test]
    fn test_multiple_cycles() {
        let edges = vec![
            // Cycle 1: a <-> b
            DependencyEdge {
                from: "a".to_string(),
                to: "b".to_string(),
                edge_type: EdgeType::Dependency,
            },
            DependencyEdge {
                from: "b".to_string(),
                to: "a".to_string(),
                edge_type: EdgeType::Include,
            },
            // Cycle 2: x -> y -> z -> x
            DependencyEdge {
                from: "x".to_string(),
                to: "y".to_string(),
                edge_type: EdgeType::Dependency,
            },
            DependencyEdge {
                from: "y".to_string(),
                to: "z".to_string(),
                edge_type: EdgeType::Dependency,
            },
            DependencyEdge {
                from: "z".to_string(),
                to: "x".to_string(),
                edge_type: EdgeType::Dependency,
            },
        ];

        let result = detect_cycles(&edges);

        assert_eq!(result.cycles.len(), 2);
        assert_eq!(result.in_cycle.len(), 5);
        assert_eq!(result.ordered.len(), 0);
    }

    #[test]
    fn test_mixed_cycle_and_dag() {
        let edges = vec![
            // DAG part
            DependencyEdge {
                from: "root".to_string(),
                to: "a".to_string(),
                edge_type: EdgeType::Dependency,
            },
            // Cycle: a -> b -> c -> a
            DependencyEdge {
                from: "a".to_string(),
                to: "b".to_string(),
                edge_type: EdgeType::Dependency,
            },
            DependencyEdge {
                from: "b".to_string(),
                to: "c".to_string(),
                edge_type: EdgeType::Include,
            },
            DependencyEdge {
                from: "c".to_string(),
                to: "a".to_string(),
                edge_type: EdgeType::ReadConfig,
            },
            // Another DAG node
            DependencyEdge {
                from: "root".to_string(),
                to: "d".to_string(),
                edge_type: EdgeType::Dependency,
            },
        ];

        let result = detect_cycles(&edges);

        assert_eq!(result.cycles.len(), 1);
        assert_eq!(result.in_cycle.len(), 3);
        assert!(result.in_cycle.contains("a"));
        assert!(result.in_cycle.contains("b"));
        assert!(result.in_cycle.contains("c"));
        assert!(!result.in_cycle.contains("root"));
        assert!(!result.in_cycle.contains("d"));

        // root and d should be in topological order
        assert!(result.ordered.contains(&"root".to_string()));
        assert!(result.ordered.contains(&"d".to_string()));
    }

    #[test]
    fn test_edge_type_weights() {
        assert_eq!(EdgeType::ReadConfig.weight(), 2);
        assert_eq!(EdgeType::Include.weight(), 3);
        assert_eq!(EdgeType::FileRead.weight(), 4);
        assert_eq!(EdgeType::TerraformSource.weight(), 5);
        assert_eq!(EdgeType::Dependency.weight(), 8);

        // Verify ordering
        assert!(EdgeType::ReadConfig.weight() < EdgeType::Include.weight());
        assert!(EdgeType::Include.weight() < EdgeType::FileRead.weight());
        assert!(EdgeType::FileRead.weight() < EdgeType::TerraformSource.weight());
        assert!(EdgeType::TerraformSource.weight() < EdgeType::Dependency.weight());
    }

    #[test]
    fn test_report_cycles_output() {
        let edges = vec![
            DependencyEdge {
                from: "a".to_string(),
                to: "b".to_string(),
                edge_type: EdgeType::Dependency,
            },
            DependencyEdge {
                from: "b".to_string(),
                to: "a".to_string(),
                edge_type: EdgeType::Include,
            },
        ];

        let result = detect_cycles(&edges);
        let mut output = Vec::new();

        report_cycles(&result, &mut output).unwrap();
        let output_str = String::from_utf8(output).unwrap();

        assert!(output_str.contains("WARNING: Dependency cycles detected"));
        assert!(output_str.contains("Cycle #1:"));
        assert!(output_str.contains("→ a"));
        assert!(output_str.contains("→ b"));
        assert!(output_str.contains("↩ back to"));
    }

    #[test]
    fn test_report_cycles_no_output_when_no_cycles() {
        let edges = vec![DependencyEdge {
            from: "a".to_string(),
            to: "b".to_string(),
            edge_type: EdgeType::Dependency,
        }];

        let result = detect_cycles(&edges);
        let mut output = Vec::new();

        report_cycles(&result, &mut output).unwrap();
        let output_str = String::from_utf8(output).unwrap();

        assert_eq!(output_str, "");
    }

    #[test]
    fn test_analyze_cycles_output() {
        let edges = vec![
            DependencyEdge {
                from: "a".to_string(),
                to: "b".to_string(),
                edge_type: EdgeType::Include,
            },
            DependencyEdge {
                from: "b".to_string(),
                to: "a".to_string(),
                edge_type: EdgeType::Dependency,
            },
        ];

        let result = detect_cycles(&edges);
        let mut output = Vec::new();

        analyze_cycles(&result, &edges, &mut output).unwrap();
        let output_str = String::from_utf8(output).unwrap();

        assert!(output_str.contains("🔍 Cycle Analysis"));
        assert!(output_str.contains("Found 1 cycle(s)"));
        assert!(output_str.contains("Cycle #1"));
        assert!(output_str.contains("Path:"));
        assert!(output_str.contains("in-degree:"));
        assert!(output_str.contains("out-degree:"));
        assert!(output_str.contains("Edges (sorted by break priority):"));
        assert!(output_str.contains("SUGGESTED BREAK"));
    }

    #[test]
    fn test_analyze_cycles_no_cycles() {
        let edges = vec![DependencyEdge {
            from: "a".to_string(),
            to: "b".to_string(),
            edge_type: EdgeType::Dependency,
        }];

        let result = detect_cycles(&edges);
        let mut output = Vec::new();

        analyze_cycles(&result, &edges, &mut output).unwrap();
        let output_str = String::from_utf8(output).unwrap();

        assert!(output_str.contains("✓ No cycles detected"));
    }

    #[test]
    fn test_empty_graph() {
        let edges: Vec<DependencyEdge> = vec![];
        let result = detect_cycles(&edges);

        assert_eq!(result.cycles.len(), 0);
        assert_eq!(result.in_cycle.len(), 0);
        assert_eq!(result.ordered.len(), 0);
    }

    #[test]
    fn test_self_loop() {
        let edges = vec![DependencyEdge {
            from: "a".to_string(),
            to: "a".to_string(),
            edge_type: EdgeType::Dependency,
        }];

        let result = detect_cycles(&edges);

        assert_eq!(result.cycles.len(), 1);
        assert_eq!(result.in_cycle.len(), 1);
        assert!(result.in_cycle.contains("a"));

        let cycle = &result.cycles[0];
        assert_eq!(cycle.nodes.len(), 1);
        assert_eq!(cycle.nodes[0], "a");
    }
}
