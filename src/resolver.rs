//! Resolve paths and evaluate terragrunt functions.

use camino::Utf8PathBuf;

/// Resolve `find_in_parent_folders()` from the given directory
pub fn find_in_parent_folders(
    _from: &camino::Utf8Path,
    _filename: Option<&str>,
) -> Option<Utf8PathBuf> {
    todo!("Implement find_in_parent_folders")
}

/// Get the repository root (directory containing .git)
pub fn get_repo_root(_from: &camino::Utf8Path) -> Option<Utf8PathBuf> {
    todo!("Implement get_repo_root")
}
