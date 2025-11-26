//! Parse HCL files and extract terragrunt constructs.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ParseError {
    #[error("Failed to parse HCL: {0}")]
    HclError(String),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

/// Parsed terragrunt configuration
#[derive(Debug, Default)]
pub struct TerragruntConfig {
    pub dependencies: Vec<String>,
    pub includes: Vec<String>,
    pub terraform_source: Option<String>,
    pub watch_files: Vec<String>,
}

/// Parse a terragrunt.hcl file
pub fn parse_terragrunt_file(_path: &camino::Utf8Path) -> Result<TerragruntConfig, ParseError> {
    todo!("Implement HCL parsing")
}
