//! Project representation and dependency collection.

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

/// A terragrunt project with its dependencies and watch files
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    /// Project name (derived from path)
    pub name: String,
    /// Directory containing terragrunt.hcl
    pub dir: Utf8PathBuf,
    /// Other projects this depends on
    pub project_dependencies: Vec<String>,
    /// Files/directories to watch for changes
    pub watch_files: Vec<Utf8PathBuf>,
}
