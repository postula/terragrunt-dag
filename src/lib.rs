pub mod discovery;
pub mod output;
pub mod parser;
pub mod processor;
pub mod project;
pub mod resolver;

// Re-export main types
pub use processor::{ProjectResult, process_projects};
pub use project::Project;
