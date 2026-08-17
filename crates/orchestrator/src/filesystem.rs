use std::fs;
use std::path::{Path, PathBuf};

use crate::{OrchestratorError, Result};

#[derive(Debug, Clone)]
pub struct WorkspaceFs {
    root: PathBuf,
}

impl WorkspaceFs {
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            root: root.as_ref().canonicalize()?,
        })
    }

    pub fn resolve(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        let candidate = self.root.join(relative);
        let canonical = candidate.canonicalize()?;
        if !canonical.starts_with(&self.root) {
            return Err(OrchestratorError::AccessDenied(
                canonical.display().to_string(),
            ));
        }
        Ok(canonical)
    }

    pub fn read(&self, relative: impl AsRef<Path>) -> Result<Vec<u8>> {
        Ok(fs::read(self.resolve(relative)?)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_inside_workspace() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("hello.txt"), "hello").unwrap();
        let workspace = WorkspaceFs::new(dir.path()).unwrap();
        assert_eq!(workspace.read("hello.txt").unwrap(), b"hello");
    }
}
