//! Parse HCL files and extract terragrunt constructs.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ParseError {
    #[error("Failed to parse HCL: {0}")]
    HclError(String),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

/// Represents a path expression extracted from HCL.
/// Can be a literal string or a function call that needs resolution.
#[derive(Debug, Clone, PartialEq)]
pub enum PathExpr {
    /// Literal string path
    Literal(String),
    /// find_in_parent_folders() or find_in_parent_folders("filename")
    FindInParentFolders(Option<String>),
    /// get_repo_root()
    GetRepoRoot,
    /// get_terragrunt_dir()
    GetTerragruntDir,
    /// get_parent_terragrunt_dir()
    GetParentTerragruntDir,
    /// path_relative_to_include()
    PathRelativeToInclude,
    /// path_relative_from_include()
    PathRelativeFromInclude,
    /// dirname(path) - extract parent directory
    Dirname(Box<PathExpr>),
    /// format(fmt_string, args...) - sprintf-like string formatting
    /// First element is the format string, rest are arguments
    Format { fmt: String, args: Vec<PathExpr> },
    /// String interpolation: "${get_repo_root()}/modules/vpc"
    Interpolation(Vec<PathExpr>),
    /// Function we can't evaluate
    Unresolvable { func: String },
}

/// What kind of dependency is this
#[derive(Debug, Clone, PartialEq)]
pub enum DependencyKind {
    /// dependency/dependencies block → other project
    Project,
    /// include block → recurse into file
    Include,
    /// terraform.source (local) → watch directory
    TerraformSource,
    /// read_terragrunt_config() → recurse into file
    ReadConfig,
    /// file(), sops_decrypt_file(), read_tfvars_file() → watch file
    FileRead,
}

/// A dependency extracted from HCL
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedDep {
    /// What kind of dependency
    pub kind: DependencyKind,
    /// The path expression (may need resolution)
    pub path: PathExpr,
    /// Optional name (e.g., dependency block label)
    pub name: Option<String>,
    /// Whether this dependency is enabled (None = not specified or expression)
    pub enabled: Option<bool>,
}

/// Parsed terragrunt configuration
#[derive(Debug, Default)]
pub struct TerragruntConfig {
    /// All extracted dependencies (projects, includes, watch files, etc.)
    pub deps: Vec<ExtractedDep>,
}

/// Parse a terragrunt.hcl file
pub fn parse_terragrunt_file(path: &camino::Utf8Path) -> Result<TerragruntConfig, ParseError> {
    let content = std::fs::read_to_string(path)?;
    let body: hcl::Body =
        hcl::from_str(&content).map_err(|e| ParseError::HclError(e.to_string()))?;

    let mut config = TerragruntConfig::default();

    // Extract dependency blocks
    for block in body.blocks() {
        match block.identifier() {
            "dependency" => {
                if let Some(dep) = extract_dependency_block(block) {
                    config.deps.push(dep);
                }
            }
            "dependencies" => {
                let deps = extract_dependencies_block(block);
                config.deps.extend(deps);
            }
            "include" => {
                if let Some(dep) = extract_include_block(block) {
                    config.deps.push(dep);
                }
            }
            "terraform" => {
                if let Some(dep) = extract_terraform_block(block) {
                    config.deps.push(dep);
                }
            }
            "locals" => {
                let deps = extract_locals_block(block);
                config.deps.extend(deps);
            }
            _ => {}
        }
    }

    Ok(config)
}

/// Extract a dependency block into ExtractedDep
fn extract_dependency_block(block: &hcl::Block) -> Option<ExtractedDep> {
    // Get the block label (dependency name)
    let name = block.labels().first().map(|l| l.as_str().to_string());

    // Get config_path attribute
    let path_expr = block
        .body()
        .attributes()
        .find(|attr| attr.key() == "config_path")
        .map(|attr| extract_path_expr(attr.expr()))?;

    // Get enabled attribute if present
    let enabled = block
        .body()
        .attributes()
        .find(|attr| attr.key() == "enabled")
        .and_then(|attr| match attr.expr() {
            hcl::Expression::Bool(b) => Some(*b),
            _ => None,
        });

    Some(ExtractedDep {
        kind: DependencyKind::Project,
        path: path_expr,
        name,
        enabled,
    })
}

/// Extract dependencies from a plural `dependencies` block.
/// Returns multiple ExtractedDep entries (one per path in the array).
fn extract_dependencies_block(block: &hcl::Block) -> Vec<ExtractedDep> {
    // Find the "paths" attribute
    let paths_attr = block.body().attributes().find(|attr| attr.key() == "paths");

    let Some(attr) = paths_attr else {
        return vec![];
    };

    // Extract array elements
    let hcl::Expression::Array(items) = attr.expr() else {
        return vec![];
    };

    items
        .iter()
        .map(|expr| ExtractedDep {
            kind: DependencyKind::Project,
            path: extract_path_expr(expr),
            name: None,    // dependencies block has no labels
            enabled: None, // dependencies block doesn't support per-path enabled
        })
        .collect()
}

/// Extract an include block into ExtractedDep
fn extract_include_block(block: &hcl::Block) -> Option<ExtractedDep> {
    // Get the block label (include name)
    let name = block.labels().first().map(|l| l.as_str().to_string());

    // Get path attribute
    let path_expr = block
        .body()
        .attributes()
        .find(|attr| attr.key() == "path")
        .map(|attr| extract_path_expr(attr.expr()))?;

    Some(ExtractedDep {
        kind: DependencyKind::Include,
        path: path_expr,
        name,
        enabled: None, // include doesn't support enabled
    })
}

/// Extract terraform source from a terraform block.
/// Only extracts LOCAL sources - remote sources (git::, tfr://, github.com/) are ignored.
fn extract_terraform_block(block: &hcl::Block) -> Option<ExtractedDep> {
    // Get source attribute
    let source_attr = block
        .body()
        .attributes()
        .find(|attr| attr.key() == "source")?;

    let path_expr = extract_path_expr(source_attr.expr());

    // Check if this is a remote source that should be ignored
    if is_remote_source(&path_expr) {
        return None;
    }

    Some(ExtractedDep {
        kind: DependencyKind::TerraformSource,
        path: path_expr,
        name: None, // terraform block has no label
        enabled: None,
    })
}

/// Check if a path expression represents a remote source that should be ignored.
fn is_remote_source(path: &PathExpr) -> bool {
    match path {
        PathExpr::Literal(s) => {
            // Remote source patterns to ignore
            s.starts_with("git::")
                || s.starts_with("tfr://")
                || s.starts_with("tfr:///")
                || s.starts_with("github.com/")
                || s.starts_with("bitbucket.org/")
                || s.starts_with("registry.terraform.io/")
                || s.contains("://") // Any URL scheme
        }
        // Function calls and interpolations are assumed to be local
        // (e.g., ${get_repo_root()}/modules/vpc)
        _ => false,
    }
}

/// Extract file dependencies from a locals block.
/// Recursively walks all expressions to find file-related function calls.
fn extract_locals_block(block: &hcl::Block) -> Vec<ExtractedDep> {
    let mut deps = Vec::new();

    // Iterate over all attributes in the locals block
    for attr in block.body().attributes() {
        // Recursively extract function calls from the expression
        extract_file_functions_from_expr(attr.expr(), &mut deps);
    }

    deps
}

/// Recursively walk an HCL expression and extract file-related function calls.
fn extract_file_functions_from_expr(expr: &hcl::Expression, deps: &mut Vec<ExtractedDep>) {
    match expr {
        hcl::Expression::FuncCall(func_call) => {
            let func_name = func_call.name.name.as_str();

            // Check if this is a file-related function
            match func_name {
                "read_terragrunt_config" => {
                    if let Some(path_expr) = extract_first_arg_as_path(func_call) {
                        deps.push(ExtractedDep {
                            kind: DependencyKind::ReadConfig,
                            path: path_expr,
                            name: None,
                            enabled: None,
                        });
                    }
                }
                "file" | "sops_decrypt_file" | "read_tfvars_file" => {
                    if let Some(path_expr) = extract_first_arg_as_path(func_call) {
                        deps.push(ExtractedDep {
                            kind: DependencyKind::FileRead,
                            path: path_expr,
                            name: None,
                            enabled: None,
                        });
                    }
                }
                _ => {
                    // Not a file function, but still recurse into arguments
                    // (e.g., yamldecode(file(...)) - we need to find the file() inside)
                }
            }

            // Always recurse into function arguments to find nested calls
            for arg in &func_call.args {
                extract_file_functions_from_expr(arg, deps);
            }
        }

        // Recurse into array elements
        hcl::Expression::Array(items) => {
            for item in items {
                extract_file_functions_from_expr(item, deps);
            }
        }

        // Recurse into object values
        hcl::Expression::Object(obj) => {
            for (_, value) in obj.iter() {
                extract_file_functions_from_expr(value, deps);
            }
        }

        // Recurse into parenthesized expressions
        hcl::Expression::Parenthesis(inner) => {
            extract_file_functions_from_expr(inner, deps);
        }

        // Recurse into conditional expressions (condition ? true_val : false_val)
        hcl::Expression::Conditional(cond) => {
            extract_file_functions_from_expr(&cond.cond_expr, deps);
            extract_file_functions_from_expr(&cond.true_expr, deps);
            extract_file_functions_from_expr(&cond.false_expr, deps);
        }

        // Recurse into binary and unary operations
        hcl::Expression::Operation(op) => match &**op {
            hcl::Operation::Unary(unary) => {
                extract_file_functions_from_expr(&unary.expr, deps);
            }
            hcl::Operation::Binary(binary) => {
                extract_file_functions_from_expr(&binary.lhs_expr, deps);
                extract_file_functions_from_expr(&binary.rhs_expr, deps);
            }
        },

        // Recurse into for expressions
        hcl::Expression::ForExpr(for_expr) => {
            extract_file_functions_from_expr(&for_expr.collection_expr, deps);
            if let Some(key_expr) = &for_expr.key_expr {
                extract_file_functions_from_expr(key_expr, deps);
            }
            extract_file_functions_from_expr(&for_expr.value_expr, deps);
            if let Some(cond) = &for_expr.cond_expr {
                extract_file_functions_from_expr(cond, deps);
            }
        }

        // Leaf expressions - no recursion needed
        hcl::Expression::Null
        | hcl::Expression::Bool(_)
        | hcl::Expression::Number(_)
        | hcl::Expression::String(_)
        | hcl::Expression::Variable(_)
        | hcl::Expression::Traversal(_)
        | hcl::Expression::TemplateExpr(_) => {
            // No nested expressions to extract
        }

        // Wildcard for any future expression types added to hcl-rs
        _ => {
            // No recursion for unknown expression types
        }
    }
}

/// Extract the first argument of a function call as a PathExpr.
fn extract_first_arg_as_path(func_call: &hcl::expr::FuncCall) -> Option<PathExpr> {
    func_call.args.first().map(extract_path_expr)
}

/// Extract a PathExpr from an HCL expression
fn extract_path_expr(expr: &hcl::Expression) -> PathExpr {
    match expr {
        hcl::Expression::String(s) => PathExpr::Literal(s.clone()),

        hcl::Expression::FuncCall(func_call) => {
            let func_name = func_call.name.name.as_str();
            match func_name {
                "find_in_parent_folders" => {
                    // Extract optional filename argument
                    let arg = func_call.args.first().and_then(|arg| match arg {
                        hcl::Expression::String(s) => Some(s.clone()),
                        _ => None,
                    });
                    PathExpr::FindInParentFolders(arg)
                }
                "get_repo_root" => PathExpr::GetRepoRoot,
                "get_terragrunt_dir" => PathExpr::GetTerragruntDir,
                "get_parent_terragrunt_dir" => PathExpr::GetParentTerragruntDir,
                "path_relative_to_include" => PathExpr::PathRelativeToInclude,
                "path_relative_from_include" => PathExpr::PathRelativeFromInclude,
                "dirname" => {
                    // dirname(path) - extract the first argument and wrap it
                    if let Some(arg) = func_call.args.first() {
                        PathExpr::Dirname(Box::new(extract_path_expr(arg)))
                    } else {
                        PathExpr::Unresolvable {
                            func: "dirname with no arguments".to_string(),
                        }
                    }
                }
                "format" => {
                    // format(fmt_string, arg1, arg2, ...) - sprintf-like formatting
                    // First argument should be a format string with %s placeholders
                    let mut args_iter = func_call.args.iter();

                    // Get the format string (first argument)
                    if let Some(fmt_arg) = args_iter.next() {
                        if let hcl::Expression::String(fmt) = fmt_arg {
                            // Collect remaining arguments as PathExprs
                            let args: Vec<PathExpr> = args_iter.map(extract_path_expr).collect();
                            PathExpr::Format {
                                fmt: fmt.clone(),
                                args,
                            }
                        } else {
                            // Format string is not a literal - try template expression
                            PathExpr::Unresolvable {
                                func: "format with non-literal format string".to_string(),
                            }
                        }
                    } else {
                        PathExpr::Unresolvable {
                            func: "format with no arguments".to_string(),
                        }
                    }
                }
                _ => PathExpr::Unresolvable {
                    func: func_name.to_string(),
                },
            }
        }

        hcl::Expression::TemplateExpr(template) => {
            // Template expressions like "${get_repo_root()}/modules/vpc"
            // Parse the template into an Interpolation with multiple parts
            extract_template_expr(template)
        }

        // For other expressions, mark as unresolvable
        _ => PathExpr::Unresolvable {
            func: format!("{:?}", expr),
        },
    }
}

/// Extract a template expression into an Interpolation PathExpr.
/// Templates consist of literal strings and interpolated expressions: "${expr}".
fn extract_template_expr(template_expr: &hcl::expr::TemplateExpr) -> PathExpr {
    // Parse the TemplateExpr into a Template
    let template = match hcl::Template::from_expr(template_expr) {
        Ok(t) => t,
        Err(_) => {
            // If parsing fails, mark as unresolvable
            return PathExpr::Unresolvable {
                func: format!("template parse error: {}", template_expr),
            };
        }
    };

    let mut parts = Vec::new();

    // Template has elements() method that returns &[Element]
    for element in template.elements() {
        match element {
            hcl::template::Element::Literal(s) => {
                // Only add non-empty literals
                if !s.is_empty() {
                    parts.push(PathExpr::Literal(s.clone()));
                }
            }
            hcl::template::Element::Interpolation(interp) => {
                // Interpolation has an expr field (not a method)
                parts.push(extract_path_expr(&interp.expr));
            }
            hcl::template::Element::Directive(_) => {
                // Directives (%{...}) are not supported for path expressions
                parts.push(PathExpr::Unresolvable {
                    func: "template directive".to_string(),
                });
            }
        }
    }

    if parts.is_empty() {
        // Empty template - shouldn't happen but handle it
        PathExpr::Unresolvable {
            func: "empty template".to_string(),
        }
    } else if parts.len() == 1 {
        // Single part - unwrap it (optimization)
        parts.into_iter().next().unwrap()
    } else {
        // Multiple parts - this is an interpolation
        PathExpr::Interpolation(parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    fn fixture_path(name: &str) -> Utf8PathBuf {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        Utf8PathBuf::from(manifest_dir)
            .join("tests/fixtures/parser")
            .join(name)
            .join("terragrunt.hcl")
    }

    #[test]
    fn test_parse_simple_dependency() {
        let path = fixture_path("simple_dependency");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        assert_eq!(config.deps.len(), 1);

        let dep = &config.deps[0];
        assert_eq!(dep.kind, DependencyKind::Project);
        assert_eq!(dep.path, PathExpr::Literal("../vpc".to_string()));
        assert_eq!(dep.name, Some("vpc".to_string()));
        assert_eq!(dep.enabled, None);
    }

    #[test]
    fn test_parse_dependencies_single_path() {
        let path = fixture_path("dependencies_single");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        assert_eq!(config.deps.len(), 1);

        let dep = &config.deps[0];
        assert_eq!(dep.kind, DependencyKind::Project);
        assert_eq!(dep.path, PathExpr::Literal("../vpc".to_string()));
        assert_eq!(dep.name, None); // dependencies block has no label
        assert_eq!(dep.enabled, None);
    }

    #[test]
    fn test_parse_dependencies_multiple_paths() {
        let path = fixture_path("dependencies_multiple");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        assert_eq!(config.deps.len(), 3);

        // Verify all paths are extracted
        let paths: Vec<&PathExpr> = config.deps.iter().map(|d| &d.path).collect();
        assert!(paths.contains(&&PathExpr::Literal("../vpc".to_string())));
        assert!(paths.contains(&&PathExpr::Literal("../sg".to_string())));
        assert!(paths.contains(&&PathExpr::Literal("../rds".to_string())));

        // All should be Project kind with no name
        for dep in &config.deps {
            assert_eq!(dep.kind, DependencyKind::Project);
            assert_eq!(dep.name, None);
        }
    }

    #[test]
    fn test_parse_dependencies_empty_paths() {
        let path = fixture_path("dependencies_empty");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        assert_eq!(config.deps.len(), 0);
    }

    #[test]
    fn test_parse_mixed_dependency_and_dependencies() {
        let path = fixture_path("mixed_dependency_types");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        // 2 from dependency blocks + 2 from dependencies block = 4 total
        assert_eq!(config.deps.len(), 4);

        // Check singular dependency blocks (have names)
        let named_deps: Vec<_> = config.deps.iter().filter(|d| d.name.is_some()).collect();
        assert_eq!(named_deps.len(), 2);

        // Find the "vpc" dependency
        let vpc = named_deps
            .iter()
            .find(|d| d.name == Some("vpc".to_string()))
            .unwrap();
        assert_eq!(vpc.path, PathExpr::Literal("../vpc".to_string()));
        assert_eq!(vpc.enabled, None);

        // Find the "sg" dependency (disabled)
        let sg = named_deps
            .iter()
            .find(|d| d.name == Some("sg".to_string()))
            .unwrap();
        assert_eq!(sg.path, PathExpr::Literal("../sg".to_string()));
        assert_eq!(sg.enabled, Some(false));

        // Check plural dependencies block entries (no names)
        let unnamed_deps: Vec<_> = config.deps.iter().filter(|d| d.name.is_none()).collect();
        assert_eq!(unnamed_deps.len(), 2);

        let unnamed_paths: Vec<&PathExpr> = unnamed_deps.iter().map(|d| &d.path).collect();
        assert!(unnamed_paths.contains(&&PathExpr::Literal("../rds".to_string())));
        assert!(unnamed_paths.contains(&&PathExpr::Literal("../elasticache".to_string())));
    }

    #[test]
    fn test_parse_include_literal_path() {
        let path = fixture_path("include_literal");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        assert_eq!(config.deps.len(), 1);

        let dep = &config.deps[0];
        assert_eq!(dep.kind, DependencyKind::Include);
        assert_eq!(dep.path, PathExpr::Literal("../root.hcl".to_string()));
        assert_eq!(dep.name, Some("root".to_string()));
        assert_eq!(dep.enabled, None);
    }

    #[test]
    fn test_parse_include_find_in_parent_folders() {
        let path = fixture_path("include_find_in_parent_folders");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        assert_eq!(config.deps.len(), 1);

        let dep = &config.deps[0];
        assert_eq!(dep.kind, DependencyKind::Include);
        assert_eq!(dep.path, PathExpr::FindInParentFolders(None));
        assert_eq!(dep.name, Some("root".to_string()));
    }

    #[test]
    fn test_parse_include_find_in_parent_folders_with_filename() {
        let path = fixture_path("include_find_in_parent_folders_with_arg");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        assert_eq!(config.deps.len(), 1);

        let dep = &config.deps[0];
        assert_eq!(dep.kind, DependencyKind::Include);
        assert_eq!(
            dep.path,
            PathExpr::FindInParentFolders(Some("env.hcl".to_string()))
        );
        assert_eq!(dep.name, Some("env".to_string()));
    }

    #[test]
    fn test_parse_multiple_includes() {
        let path = fixture_path("include_multiple");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        assert_eq!(config.deps.len(), 3);

        // All should be Include kind
        assert!(
            config
                .deps
                .iter()
                .all(|d| d.kind == DependencyKind::Include)
        );

        // Find each by name
        let root = config
            .deps
            .iter()
            .find(|d| d.name == Some("root".to_string()))
            .unwrap();
        assert_eq!(root.path, PathExpr::FindInParentFolders(None));

        let env = config
            .deps
            .iter()
            .find(|d| d.name == Some("env".to_string()))
            .unwrap();
        assert_eq!(env.path, PathExpr::Literal("../env.hcl".to_string()));

        let region = config
            .deps
            .iter()
            .find(|d| d.name == Some("region".to_string()))
            .unwrap();
        assert_eq!(
            region.path,
            PathExpr::FindInParentFolders(Some("region.hcl".to_string()))
        );
    }

    #[test]
    fn test_parse_include_with_other_blocks() {
        let path = fixture_path("include_with_dependencies");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        // 1 include + 1 dependency + 1 from dependencies block = 3
        assert_eq!(config.deps.len(), 3);

        // Count by kind
        let includes: Vec<_> = config
            .deps
            .iter()
            .filter(|d| d.kind == DependencyKind::Include)
            .collect();
        let projects: Vec<_> = config
            .deps
            .iter()
            .filter(|d| d.kind == DependencyKind::Project)
            .collect();

        assert_eq!(includes.len(), 1);
        assert_eq!(projects.len(), 2);

        // Verify include
        assert_eq!(includes[0].name, Some("root".to_string()));
        assert_eq!(includes[0].path, PathExpr::FindInParentFolders(None));
    }

    #[test]
    fn test_parse_terraform_local_source() {
        let path = fixture_path("terraform_local_source");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        assert_eq!(config.deps.len(), 1);

        let dep = &config.deps[0];
        assert_eq!(dep.kind, DependencyKind::TerraformSource);
        assert_eq!(dep.path, PathExpr::Literal("../modules/vpc".to_string()));
        assert_eq!(dep.name, None); // terraform block has no label
    }

    #[test]
    fn test_parse_terraform_local_source_with_interpolation() {
        let path = fixture_path("terraform_local_source_absolute");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        assert_eq!(config.deps.len(), 1);

        let dep = &config.deps[0];
        assert_eq!(dep.kind, DependencyKind::TerraformSource);
        // This should be an interpolation or function call - for now accept either representation
        // The key is that it's NOT ignored
        assert!(matches!(
            dep.path,
            PathExpr::Interpolation(_) | PathExpr::Unresolvable { .. }
        ));
    }

    #[test]
    fn test_parse_terraform_git_source_ignored() {
        let path = fixture_path("terraform_git_source");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        // Git sources should be ignored - no deps extracted
        assert_eq!(config.deps.len(), 0);
    }

    #[test]
    fn test_parse_terraform_tfr_source_ignored() {
        let path = fixture_path("terraform_tfr_source");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        // Terraform registry sources should be ignored
        assert_eq!(config.deps.len(), 0);
    }

    #[test]
    fn test_parse_terraform_github_source_ignored() {
        let path = fixture_path("terraform_github_source");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        // GitHub shorthand sources should be ignored
        assert_eq!(config.deps.len(), 0);
    }

    #[test]
    fn test_parse_terraform_with_other_blocks() {
        let path = fixture_path("terraform_with_other_blocks");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        // 1 include + 1 terraform source + 1 dependency = 3
        assert_eq!(config.deps.len(), 3);

        // Count by kind
        let terraform_sources: Vec<_> = config
            .deps
            .iter()
            .filter(|d| d.kind == DependencyKind::TerraformSource)
            .collect();
        assert_eq!(terraform_sources.len(), 1);
        assert_eq!(
            terraform_sources[0].path,
            PathExpr::Literal("../modules/app".to_string())
        );

        let includes: Vec<_> = config
            .deps
            .iter()
            .filter(|d| d.kind == DependencyKind::Include)
            .collect();
        assert_eq!(includes.len(), 1);

        let projects: Vec<_> = config
            .deps
            .iter()
            .filter(|d| d.kind == DependencyKind::Project)
            .collect();
        assert_eq!(projects.len(), 1);
    }

    #[test]
    fn test_parse_terraform_no_source() {
        let path = fixture_path("terraform_no_source");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        // terraform block without source should not add any deps
        assert_eq!(config.deps.len(), 0);
    }

    // ============== locals block tests ==============

    #[test]
    fn test_parse_locals_read_terragrunt_config() {
        let path = fixture_path("locals_read_terragrunt_config");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        assert_eq!(config.deps.len(), 1);

        let dep = &config.deps[0];
        assert_eq!(dep.kind, DependencyKind::ReadConfig);
        assert_eq!(dep.path, PathExpr::Literal("../common.hcl".to_string()));
        assert_eq!(dep.name, None);
    }

    #[test]
    fn test_parse_locals_file() {
        let path = fixture_path("locals_file");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        assert_eq!(config.deps.len(), 1);

        let dep = &config.deps[0];
        assert_eq!(dep.kind, DependencyKind::FileRead);
        assert_eq!(dep.path, PathExpr::Literal("../config.yaml".to_string()));
    }

    #[test]
    fn test_parse_locals_sops_decrypt_file() {
        let path = fixture_path("locals_sops_decrypt_file");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        assert_eq!(config.deps.len(), 1);

        let dep = &config.deps[0];
        assert_eq!(dep.kind, DependencyKind::FileRead);
        assert_eq!(dep.path, PathExpr::Literal("../secrets.yaml".to_string()));
    }

    #[test]
    fn test_parse_locals_read_tfvars_file() {
        let path = fixture_path("locals_read_tfvars_file");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        assert_eq!(config.deps.len(), 1);

        let dep = &config.deps[0];
        assert_eq!(dep.kind, DependencyKind::FileRead);
        assert_eq!(
            dep.path,
            PathExpr::Literal("../terraform.tfvars".to_string())
        );
    }

    #[test]
    fn test_parse_locals_multiple_functions() {
        let path = fixture_path("locals_multiple_functions");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        // 2 read_terragrunt_config + 1 file + 1 sops_decrypt_file + 1 read_tfvars_file = 5
        assert_eq!(config.deps.len(), 5);

        // Count by kind
        let read_configs: Vec<_> = config
            .deps
            .iter()
            .filter(|d| d.kind == DependencyKind::ReadConfig)
            .collect();
        assert_eq!(read_configs.len(), 2);

        let file_reads: Vec<_> = config
            .deps
            .iter()
            .filter(|d| d.kind == DependencyKind::FileRead)
            .collect();
        assert_eq!(file_reads.len(), 3);

        // Verify specific paths
        let paths: Vec<&PathExpr> = config.deps.iter().map(|d| &d.path).collect();
        assert!(paths.contains(&&PathExpr::Literal("../common.hcl".to_string())));
        assert!(paths.contains(&&PathExpr::Literal("../env.hcl".to_string())));
        assert!(paths.contains(&&PathExpr::Literal("../config.yaml".to_string())));
        assert!(paths.contains(&&PathExpr::Literal("../secrets.yaml".to_string())));
        assert!(paths.contains(&&PathExpr::Literal("../terraform.tfvars".to_string())));
    }

    #[test]
    fn test_parse_locals_nested_function_calls() {
        let path = fixture_path("locals_nested_function_calls");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        assert_eq!(config.deps.len(), 1);

        let dep = &config.deps[0];
        assert_eq!(dep.kind, DependencyKind::ReadConfig);
        // The path argument is find_in_parent_folders("root.hcl")
        assert_eq!(
            dep.path,
            PathExpr::FindInParentFolders(Some("root.hcl".to_string()))
        );
    }

    #[test]
    fn test_parse_locals_with_other_blocks() {
        let path = fixture_path("locals_with_other_blocks");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        // 1 include + 2 from locals + 1 dependency + 1 terraform = 5
        assert_eq!(config.deps.len(), 5);

        // Verify we have all kinds
        assert!(
            config
                .deps
                .iter()
                .any(|d| d.kind == DependencyKind::Include)
        );
        assert!(
            config
                .deps
                .iter()
                .any(|d| d.kind == DependencyKind::ReadConfig)
        );
        assert!(
            config
                .deps
                .iter()
                .any(|d| d.kind == DependencyKind::FileRead)
        );
        assert!(
            config
                .deps
                .iter()
                .any(|d| d.kind == DependencyKind::Project)
        );
        assert!(
            config
                .deps
                .iter()
                .any(|d| d.kind == DependencyKind::TerraformSource)
        );
    }

    #[test]
    fn test_parse_locals_no_file_functions() {
        let path = fixture_path("locals_no_file_functions");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        // No file-related functions, should be empty
        assert_eq!(config.deps.len(), 0);
    }

    #[test]
    fn test_parse_locals_deeply_nested() {
        let path = fixture_path("locals_deeply_nested");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        // Should find both file() calls even though deeply nested
        assert_eq!(config.deps.len(), 2);

        assert!(
            config
                .deps
                .iter()
                .all(|d| d.kind == DependencyKind::FileRead)
        );

        let paths: Vec<&PathExpr> = config.deps.iter().map(|d| &d.path).collect();
        assert!(paths.contains(&&PathExpr::Literal("../base.yaml".to_string())));
        assert!(paths.contains(&&PathExpr::Literal("../override.yaml".to_string())));
    }

    // ============== Template expression tests ==============

    #[test]
    fn test_parse_include_template_expr_with_dirname() {
        let path = fixture_path("include_template_expr");
        let config = parse_terragrunt_file(&path).expect("should parse successfully");

        assert_eq!(config.deps.len(), 1);

        let dep = &config.deps[0];
        assert_eq!(dep.kind, DependencyKind::Include);
        assert_eq!(dep.name, Some("envcommon".to_string()));

        // The path should be an Interpolation with two parts:
        // 1. Dirname(FindInParentFolders("root.hcl"))
        // 2. Literal("/_envcommon/service.hcl")
        match &dep.path {
            PathExpr::Interpolation(parts) => {
                assert_eq!(parts.len(), 2);

                // First part: dirname(find_in_parent_folders("root.hcl"))
                match &parts[0] {
                    PathExpr::Dirname(inner) => match &**inner {
                        PathExpr::FindInParentFolders(Some(filename)) => {
                            assert_eq!(filename, "root.hcl");
                        }
                        _ => panic!("Expected FindInParentFolders inside Dirname"),
                    },
                    _ => panic!("Expected Dirname as first part"),
                }

                // Second part: literal "/_envcommon/service.hcl"
                assert_eq!(
                    parts[1],
                    PathExpr::Literal("/_envcommon/service.hcl".to_string())
                );
            }
            _ => panic!("Expected Interpolation, got {:?}", dep.path),
        }
    }

    #[test]
    fn test_extract_path_expr_template_simple() {
        // Test direct extraction of a simple template expression
        let hcl_str = r#"test = "${get_repo_root()}/modules/vpc""#;
        let body: hcl::Body = hcl::from_str(hcl_str).unwrap();
        let attr = body.attributes().next().unwrap();

        let path_expr = extract_path_expr(attr.expr());

        match path_expr {
            PathExpr::Interpolation(parts) => {
                assert_eq!(parts.len(), 2);
                assert_eq!(parts[0], PathExpr::GetRepoRoot);
                assert_eq!(parts[1], PathExpr::Literal("/modules/vpc".to_string()));
            }
            _ => panic!("Expected Interpolation, got {:?}", path_expr),
        }
    }

    #[test]
    fn test_extract_path_expr_template_complex() {
        // Test a more complex template with nested functions
        let hcl_str = r#"test = "${dirname(find_in_parent_folders("root.hcl"))}/_envcommon/${get_terragrunt_dir()}.hcl""#;
        let body: hcl::Body = hcl::from_str(hcl_str).unwrap();
        let attr = body.attributes().next().unwrap();

        let path_expr = extract_path_expr(attr.expr());

        match path_expr {
            PathExpr::Interpolation(parts) => {
                assert_eq!(parts.len(), 4);

                // First: dirname(find_in_parent_folders("root.hcl"))
                match &parts[0] {
                    PathExpr::Dirname(inner) => match &**inner {
                        PathExpr::FindInParentFolders(Some(f)) => assert_eq!(f, "root.hcl"),
                        _ => panic!("Expected FindInParentFolders in Dirname"),
                    },
                    _ => panic!("Expected Dirname"),
                }

                // Second: literal "/_envcommon/"
                assert_eq!(parts[1], PathExpr::Literal("/_envcommon/".to_string()));

                // Third: get_terragrunt_dir()
                assert_eq!(parts[2], PathExpr::GetTerragruntDir);

                // Fourth: literal ".hcl"
                assert_eq!(parts[3], PathExpr::Literal(".hcl".to_string()));
            }
            _ => panic!("Expected Interpolation, got {:?}", path_expr),
        }
    }

    #[test]
    fn test_extract_path_expr_dirname_standalone() {
        // Test extraction of dirname as a standalone function call
        let hcl_str = r#"test = dirname("/path/to/file.hcl")"#;
        let body: hcl::Body = hcl::from_str(hcl_str).unwrap();
        let attr = body.attributes().next().unwrap();

        let path_expr = extract_path_expr(attr.expr());

        match path_expr {
            PathExpr::Dirname(inner) => {
                assert_eq!(*inner, PathExpr::Literal("/path/to/file.hcl".to_string()));
            }
            _ => panic!("Expected Dirname, got {:?}", path_expr),
        }
    }

    // ============== format() function tests ==============

    #[test]
    fn test_extract_path_expr_format_simple() {
        // format("%s/alarm_topic", dirname(find_in_parent_folders("env.hcl")))
        let hcl_str =
            r#"test = format("%s/alarm_topic", dirname(find_in_parent_folders("env.hcl")))"#;
        let body: hcl::Body = hcl::from_str(hcl_str).unwrap();
        let attr = body.attributes().next().unwrap();

        let path_expr = extract_path_expr(attr.expr());

        match path_expr {
            PathExpr::Format { fmt, args } => {
                assert_eq!(fmt, "%s/alarm_topic");
                assert_eq!(args.len(), 1);

                // First arg should be dirname(find_in_parent_folders("env.hcl"))
                match &args[0] {
                    PathExpr::Dirname(inner) => match &**inner {
                        PathExpr::FindInParentFolders(Some(filename)) => {
                            assert_eq!(filename, "env.hcl");
                        }
                        _ => panic!("Expected FindInParentFolders inside Dirname"),
                    },
                    _ => panic!("Expected Dirname as first arg"),
                }
            }
            _ => panic!("Expected Format, got {:?}", path_expr),
        }
    }

    #[test]
    fn test_extract_path_expr_format_with_get_repo_root() {
        // format("%s/aws/_modules/cloudtrail", get_repo_root())
        let hcl_str = r#"test = format("%s/aws/_modules/cloudtrail", get_repo_root())"#;
        let body: hcl::Body = hcl::from_str(hcl_str).unwrap();
        let attr = body.attributes().next().unwrap();

        let path_expr = extract_path_expr(attr.expr());

        match path_expr {
            PathExpr::Format { fmt, args } => {
                assert_eq!(fmt, "%s/aws/_modules/cloudtrail");
                assert_eq!(args.len(), 1);
                assert_eq!(args[0], PathExpr::GetRepoRoot);
            }
            _ => panic!("Expected Format, got {:?}", path_expr),
        }
    }

    #[test]
    fn test_extract_path_expr_format_multiple_args() {
        // format("%s/%s/config.hcl", get_repo_root(), get_terragrunt_dir())
        let hcl_str = r#"test = format("%s/%s/config.hcl", get_repo_root(), get_terragrunt_dir())"#;
        let body: hcl::Body = hcl::from_str(hcl_str).unwrap();
        let attr = body.attributes().next().unwrap();

        let path_expr = extract_path_expr(attr.expr());

        match path_expr {
            PathExpr::Format { fmt, args } => {
                assert_eq!(fmt, "%s/%s/config.hcl");
                assert_eq!(args.len(), 2);
                assert_eq!(args[0], PathExpr::GetRepoRoot);
                assert_eq!(args[1], PathExpr::GetTerragruntDir);
            }
            _ => panic!("Expected Format, got {:?}", path_expr),
        }
    }
}
