//! JSON/YAML output serialization.

use crate::Project;
use serde::{Deserialize, Serialize};

/// Output format containing all discovered projects
#[derive(Debug, Serialize, Deserialize)]
pub struct Output {
    pub projects: Vec<Project>,
}

impl Output {
    pub fn new(projects: Vec<Project>) -> Self {
        Self { projects }
    }

    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    pub fn to_yaml(&self) -> Result<String, serde_yaml::Error> {
        serde_yaml::to_string(self)
    }
}
