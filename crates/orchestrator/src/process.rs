use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{OrchestratorError, Result};

pub type SessionId = Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ExecutionEnvironment {
    Windows,
    Wsl2 { distribution: String },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct PtySize {
    pub columns: u16,
    pub rows: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessSpec {
    pub executable: String,
    pub arguments: Vec<String>,
    pub working_directory: PathBuf,
    pub environment: ExecutionEnvironment,
    pub environment_overrides: HashMap<String, String>,
}

/// Owns child lifetimes. A platform PTY backend can implement the same lifecycle.
#[derive(Default)]
pub struct ProcessSupervisor {
    children: Mutex<HashMap<SessionId, Child>>,
}

impl ProcessSupervisor {
    pub fn spawn(&self, spec: &ProcessSpec) -> Result<SessionId> {
        if spec.executable.trim().is_empty() {
            return Err(OrchestratorError::InvalidInput("empty executable".into()));
        }
        let mut command = match &spec.environment {
            ExecutionEnvironment::Windows => Command::new(&spec.executable),
            ExecutionEnvironment::Wsl2 { distribution } => {
                let mut command = Command::new("wsl.exe");
                command.args(["--distribution", distribution, "--exec", &spec.executable]);
                command
            }
        };
        let child = command
            .args(&spec.arguments)
            .current_dir(&spec.working_directory)
            .env_clear()
            .envs(&spec.environment_overrides)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let id = Uuid::new_v4();
        self.children
            .lock()
            .expect("child lock poisoned")
            .insert(id, child);
        Ok(id)
    }

    pub fn cancel(&self, id: SessionId) -> Result<()> {
        let mut children = self.children.lock().expect("child lock poisoned");
        let child = children
            .get_mut(&id)
            .ok_or_else(|| OrchestratorError::InvalidInput("unknown session".into()))?;
        child.kill()?;
        Ok(())
    }
}
