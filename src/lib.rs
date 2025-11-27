//! terragrunt-dag - Generate dependency graph for terragrunt projects
//!
//! This crate provides tools to parse terragrunt monorepos and output
//! project dependencies in various formats (JSON, YAML, Atlantis, Digger).

pub mod discovery;
pub mod output;
pub mod parser;
pub mod processor;
pub mod project;
pub mod resolver;

// Re-export main types for convenience
pub use output::{OutputConfig, OutputError, OutputFormat, generate_output};
pub use processor::{ParseCache, ProcessError, ProjectResult, process_all_projects};
pub use project::Project;
