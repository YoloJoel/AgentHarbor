//! Trusted orchestration boundary for desktop commands.

mod approval;
mod domain;
mod filesystem;
mod persistence;
mod process;
mod recovery;
mod worktree;

pub use approval::{ApprovalDecision, ApprovalGate, Operation, Risk};
pub use domain::*;
pub use filesystem::WorkspaceFs;
pub use persistence::{JsonStateStore, RecoveryCandidate, SqliteStateStore, StateStore};
pub use process::{ExecutionEnvironment, ProcessSpec, ProcessSupervisor, PtySize, SessionId};
pub use recovery::{RecoveryRuntime, StartupRecovery, StartupRecoveryReport};
pub use worktree::{GitWorktreeService, WorktreeRequest};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum OrchestratorError {
    #[error("access denied: {0}")]
    AccessDenied(String),
    #[error("approval is required for {0}")]
    ApprovalRequired(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("state serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("database operation failed: {0}")]
    Database(#[from] rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, OrchestratorError>;
