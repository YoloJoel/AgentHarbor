use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{OrchestratorError, Result};

#[derive(Clone, Debug)]
pub struct WorktreeRequest {
    pub repository: PathBuf,
    pub destination: PathBuf,
    pub branch: String,
}

#[derive(Default)]
pub struct GitWorktreeService;

impl GitWorktreeService {
    pub fn create(&self, request: &WorktreeRequest) -> Result<()> {
        ensure_branch_name(&request.branch)?;
        let status = Command::new("git")
            .args(["worktree", "add", "-b"])
            .arg(&request.branch)
            .arg(&request.destination)
            .current_dir(&request.repository)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(OrchestratorError::InvalidInput(
                "git worktree add failed".into(),
            ))
        }
    }

    pub fn remove(&self, repository: &Path, destination: &Path) -> Result<()> {
        let status = Command::new("git")
            .args(["worktree", "remove"])
            .arg(destination)
            .current_dir(repository)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(OrchestratorError::InvalidInput(
                "git worktree remove failed".into(),
            ))
        }
    }
}

fn ensure_branch_name(branch: &str) -> Result<()> {
    if branch.is_empty() || branch.starts_with('-') || branch.contains(['\0', '\n', '\r']) {
        return Err(OrchestratorError::InvalidInput(
            "invalid branch name".into(),
        ));
    }
    Ok(())
}
