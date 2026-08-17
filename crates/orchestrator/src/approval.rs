use serde::{Deserialize, Serialize};

use crate::{OrchestratorError, Result};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Risk {
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Operation {
    pub kind: String,
    pub summary: String,
    pub risk: Risk,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalDecision {
    AllowOnce,
    Deny,
}

pub trait ApprovalGate: Send + Sync {
    fn authorize(&self, operation: &Operation) -> Result<ApprovalDecision>;
}

pub fn require_approval(gate: &dyn ApprovalGate, operation: &Operation) -> Result<()> {
    match gate.authorize(operation)? {
        ApprovalDecision::AllowOnce => Ok(()),
        ApprovalDecision::Deny => Err(OrchestratorError::AccessDenied(operation.summary.clone())),
    }
}
