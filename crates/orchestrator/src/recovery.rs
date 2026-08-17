//! Startup recovery separates durable reconciliation from platform-specific process work.

use std::path::Path;

use serde_json::json;

use crate::{AgentRunAttempt, RecoveryCandidate, Result, SqliteStateStore};

/// Implemented by the host because PID lookup, worktree mounting, context construction,
/// and adapter process launch differ between Windows and WSL.
pub trait RecoveryRuntime {
    fn is_process_alive(&self, pid: u32) -> bool;
    fn remount_worktree(&self, path: &Path) -> Result<()>;
    fn rebuild_context(&self, candidate: &RecoveryCandidate) -> Result<()>;
    fn launch(&self, candidate: &RecoveryCandidate) -> Result<u32>;
}

#[derive(Debug, Default)]
pub struct StartupRecoveryReport {
    pub interrupted: usize,
    pub resumed: Vec<AgentRunAttempt>,
}

pub struct StartupRecovery<'a> {
    store: &'a SqliteStateStore,
}

impl<'a> StartupRecovery<'a> {
    pub fn new(store: &'a SqliteStateStore) -> Self {
        Self { store }
    }

    /// Reconciles stale attempts first, then restores the worktree and checkpoint context
    /// before launching a fresh attempt under the same logical run.
    pub fn run(&self, runtime: &impl RecoveryRuntime) -> Result<StartupRecoveryReport> {
        let candidates = self
            .store
            .reconcile_running_attempts(|pid| runtime.is_process_alive(pid))?;
        let mut report = StartupRecoveryReport {
            interrupted: candidates.len(),
            resumed: Vec::new(),
        };
        for candidate in candidates {
            runtime.remount_worktree(&candidate.worktree_path)?;
            runtime.rebuild_context(&candidate)?;
            let pid = runtime.launch(&candidate)?;
            let attempt = self
                .store
                .start_attempt(candidate.agent_run_id, Some(pid))?;
            self.store.record_event(
                "agent_run",
                candidate.agent_run_id,
                "agent_run.recovered",
                &json!({
                    "from_attempt_id": candidate.interrupted_attempt_id,
                    "to_attempt_id": attempt.id,
                    "workspace_id": candidate.workspace_id,
                    "checkpoint_restored": candidate.checkpoint_context.is_some()
                }),
            )?;
            report.resumed.push(attempt);
        }
        Ok(report)
    }
}
