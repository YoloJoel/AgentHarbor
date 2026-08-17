//! Stable orchestration-domain identifiers and state values.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type EntityId = Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequirementState {
    Draft,
    Clarifying,
    Ready,
    Planning,
    Executing,
    Integrating,
    Verifying,
    Completed,
    Failed,
    Paused,
}

impl RequirementState {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Clarifying => "clarifying",
            Self::Ready => "ready",
            Self::Planning => "planning",
            Self::Executing => "executing",
            Self::Integrating => "integrating",
            Self::Verifying => "verifying",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Paused => "paused",
        }
    }

    pub(crate) fn permits(&self, next: &Self) -> bool {
        matches!(
            (self, next),
            (Self::Draft, Self::Clarifying)
                | (Self::Clarifying, Self::Ready)
                | (Self::Ready, Self::Planning)
                | (Self::Planning, Self::Executing)
                | (Self::Executing, Self::Integrating)
                | (Self::Integrating, Self::Verifying)
                | (Self::Verifying, Self::Completed)
                | (_, Self::Failed)
                | (_, Self::Paused)
                | (Self::Paused, Self::Clarifying)
                | (Self::Paused, Self::Planning)
                | (Self::Paused, Self::Executing)
        )
    }
}

macro_rules! entity {
    ($name:ident { $($field:ident: $ty:ty),* $(,)? }) => {
        #[derive(Clone, Debug, Serialize, Deserialize)]
        pub struct $name { pub id: EntityId, $(pub $field: $ty),* }
    };
}

entity!(Project { name: String });
entity!(Requirement { project_id: EntityId, title: String, state: RequirementState, confirmed_at: Option<String> });
entity!(Clarification { requirement_id: EntityId, question: String, answer: Option<String> });
entity!(Task { requirement_id: EntityId, title: String, specification_version_id: EntityId, specification_hash: String, specification_change_impact: Option<String> });
entity!(AgentRole {
    project_id: EntityId,
    name: String,
    instructions: String
});
/// Logical execution identity. Process restarts do not replace this record.
entity!(AgentRun {
    agent_role_id: EntityId,
    task_id: EntityId,
    workspace_id: EntityId
});
/// A concrete OS process belonging to a logical [`AgentRun`].
entity!(AgentRunAttempt { agent_run_id: EntityId, attempt_number: i64, pid: Option<u32>, status: AttemptStatus });
entity!(Workspace {
    project_id: EntityId,
    repository_path: PathBuf,
    worktree_path: PathBuf
});
entity!(Message {
    agent_run_id: EntityId,
    direction: String,
    body: String
});
entity!(Artifact {
    task_id: EntityId,
    workspace_id: EntityId,
    path: PathBuf,
    kind: String
});
entity!(Approval {
    agent_run_id: EntityId,
    operation: String,
    decision: String
});
entity!(Checkpoint {
    agent_run_id: EntityId,
    attempt_id: EntityId,
    sequence: i64,
    context: String
});

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStatus {
    Starting,
    Running,
    Completed,
    Failed,
    Interrupted,
}

impl AttemptStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub sequence: i64,
    pub entity_type: String,
    pub entity_id: EntityId,
    pub event_type: String,
    pub payload: serde_json::Value,
    pub occurred_at: String,
}
