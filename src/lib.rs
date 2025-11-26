pub mod discovery;
pub mod output;
pub mod parser;
pub mod processor;
pub mod project;
pub mod resolver;

// Re-export main types
pub use processor::{process_projects, ProjectResult};
pub use project::Project;
