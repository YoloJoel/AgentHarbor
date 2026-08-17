//! Durable, isolated agent contexts and content-addressed artifacts.

use crate::{OrchestratorError, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "scope", content = "id", rename_all = "snake_case")]
pub enum ContextScope {
    Requirement(Uuid),
    Lead(Uuid),
    Worker(Uuid),
}
impl ContextScope {
    fn path(&self) -> PathBuf {
        let (kind, id) = match self {
            Self::Requirement(id) => ("requirement", id),
            Self::Lead(id) => ("lead", id),
            Self::Worker(id) => ("worker", id),
        };
        PathBuf::from(kind).join(id.to_string())
    }
}

/// This section is always retained verbatim during compaction.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoreContext {
    pub specification_version_id: Uuid,
    pub specification_hash: String,
    pub acceptance_criteria: Vec<String>,
    pub architecture_decisions: Vec<String>,
    pub prohibitions: Vec<String>,
    pub task_boundaries: Vec<String>,
    pub branch: String,
    pub workspace_path: PathBuf,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactRef {
    pub hash: String,
    pub kind: String,
    pub summary: String,
    pub line_range: Option<(u64, u64)>,
    pub byte_len: u64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkingContext {
    pub current_task: String,
    pub notes: Vec<String>,
    pub artifacts: Vec<ArtifactRef>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckpointData {
    pub completed_work: Vec<String>,
    pub modified_files: Vec<PathBuf>,
    pub key_decisions: Vec<String>,
    pub failed_attempts: Vec<String>,
    pub open_questions: Vec<String>,
    pub next_steps: Vec<String>,
    pub git_status: String,
    pub workspace_head: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextCheckpoint {
    pub sequence: u64,
    pub scope: ContextScope,
    pub specification_version_id: Uuid,
    pub specification_hash: String,
    pub raw_history: ArtifactRef,
    pub data: CheckpointData,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelBudget {
    pub context_window: u64,
    pub reserved_output: u64,
    pub safety_margin: u64,
    pub soft_threshold_percent: u8,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetAssessment {
    pub usable_tokens: u64,
    pub soft_limit: u64,
    pub consumed_tokens: u64,
    pub checkpoint_required: bool,
}
impl ModelBudget {
    pub fn assess(&self, consumed_tokens: u64) -> Result<BudgetAssessment> {
        if !(1..=100).contains(&self.soft_threshold_percent) {
            return Err(OrchestratorError::InvalidInput(
                "soft threshold must be 1..=100".into(),
            ));
        }
        let reserved = self
            .reserved_output
            .checked_add(self.safety_margin)
            .ok_or_else(|| OrchestratorError::InvalidInput("token budget overflow".into()))?;
        let usable_tokens = self.context_window.checked_sub(reserved).ok_or_else(|| {
            OrchestratorError::InvalidInput("reserves exceed context window".into())
        })?;
        let soft_limit = usable_tokens.saturating_mul(self.soft_threshold_percent.into()) / 100;
        Ok(BudgetAssessment {
            usable_tokens,
            soft_limit,
            consumed_tokens,
            checkpoint_required: consumed_tokens >= soft_limit,
        })
    }
}
#[derive(Default)]
pub struct ContextBudgeter {
    models: BTreeMap<String, ModelBudget>,
}
impl ContextBudgeter {
    pub fn register(&mut self, model: impl Into<String>, budget: ModelBudget) -> Result<()> {
        budget.assess(0)?;
        self.models.insert(model.into(), budget);
        Ok(())
    }
    pub fn assess(&self, model: &str, used: u64) -> Result<BudgetAssessment> {
        self.models
            .get(model)
            .ok_or_else(|| OrchestratorError::InvalidInput(format!("unknown model: {model}")))?
            .assess(used)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RebuiltContext {
    pub core: CoreContext,
    pub checkpoint: Option<ContextCheckpoint>,
    pub current_task: String,
    pub artifacts: Vec<ArtifactRef>,
    pub git_diff: ArtifactRef,
}

/// File store whose APIs always require a scope; no complete history is shared by default.
pub struct ContextRepository {
    root: PathBuf,
}
impl ContextRepository {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("artifacts/sha256"))?;
        Ok(Self { root })
    }
    fn dir(&self, s: &ContextScope) -> PathBuf {
        self.root.join("contexts").join(s.path())
    }
    pub fn save_core(&self, s: &ContextScope, v: &CoreContext) -> Result<()> {
        self.write_json(&self.dir(s).join("core.json"), v)
    }
    pub fn save_working(&self, s: &ContextScope, v: &WorkingContext) -> Result<()> {
        self.write_json(&self.dir(s).join("working.json"), v)
    }
    pub fn put_artifact(
        &self,
        kind: impl Into<String>,
        summary: impl Into<String>,
        bytes: &[u8],
        lines: Option<(u64, u64)>,
    ) -> Result<ArtifactRef> {
        if lines.is_some_and(|(a, b)| a == 0 || b < a) {
            return Err(OrchestratorError::InvalidInput("invalid line range".into()));
        }
        let hash = format!("sha256:{:x}", Sha256::digest(bytes));
        let path = self.artifact_path(&hash)?;
        if !path.exists() {
            self.write(&path, bytes)?
        }
        Ok(ArtifactRef {
            hash,
            kind: kind.into(),
            summary: summary.into(),
            line_range: lines,
            byte_len: bytes.len() as u64,
        })
    }
    pub fn read_artifact(&self, a: &ArtifactRef) -> Result<Vec<u8>> {
        let b = fs::read(self.artifact_path(&a.hash)?)?;
        if format!("sha256:{:x}", Sha256::digest(&b)) != a.hash {
            return Err(OrchestratorError::InvalidInput(
                "artifact hash mismatch".into(),
            ));
        }
        Ok(b)
    }
    pub fn checkpoint(
        &self,
        s: &ContextScope,
        data: CheckpointData,
        raw: &[u8],
    ) -> Result<ContextCheckpoint> {
        let core: CoreContext = self.read(&self.dir(s).join("core.json"))?;
        let raw_history = self.put_artifact(
            "raw_chat_history",
            "uncompressed pre-checkpoint history",
            raw,
            None,
        )?;
        let dir = self.dir(s).join("checkpoints");
        fs::create_dir_all(&dir)?;
        let sequence = fs::read_dir(&dir)?.filter_map(|x| x.ok()).count() as u64 + 1;
        let cp = ContextCheckpoint {
            sequence,
            scope: s.clone(),
            specification_version_id: core.specification_version_id,
            specification_hash: core.specification_hash,
            raw_history,
            data,
        };
        self.write_json(&dir.join(format!("{sequence:020}.json")), &cp)?;
        Ok(cp)
    }
    pub fn rebuild(
        &self,
        s: &ContextScope,
        spec_id: Uuid,
        spec_hash: &str,
        head: &str,
        diff: &[u8],
        artifacts: &[ArtifactRef],
    ) -> Result<RebuiltContext> {
        let core: CoreContext = self.read(&self.dir(s).join("core.json"))?;
        if core.specification_version_id != spec_id || core.specification_hash != spec_hash {
            return Err(OrchestratorError::InvalidInput(
                "active specification mismatch".into(),
            ));
        }
        let working: WorkingContext = self.read(&self.dir(s).join("working.json"))?;
        let checkpoint = self.latest(s)?;
        if let Some(cp) = &checkpoint {
            if cp.specification_version_id != spec_id || cp.specification_hash != spec_hash {
                return Err(OrchestratorError::InvalidInput(
                    "checkpoint specification mismatch".into(),
                ));
            }
            if cp.data.workspace_head != head {
                return Err(OrchestratorError::InvalidInput(
                    "workspace HEAD mismatch".into(),
                ));
            }
        }
        for a in artifacts {
            self.read_artifact(a)?;
        }
        let git_diff = self.put_artifact("git_diff", "latest Git diff", diff, None)?;
        Ok(RebuiltContext {
            core,
            checkpoint,
            current_task: working.current_task,
            artifacts: artifacts.to_vec(),
            git_diff,
        })
    }
    fn latest(&self, s: &ContextScope) -> Result<Option<ContextCheckpoint>> {
        let d = self.dir(s).join("checkpoints");
        if !d.exists() {
            return Ok(None);
        }
        let mut p = fs::read_dir(d)?
            .filter_map(|x| x.ok().map(|x| x.path()))
            .collect::<Vec<_>>();
        p.sort();
        p.last().map(|x| self.read(x)).transpose()
    }
    fn artifact_path(&self, h: &str) -> Result<PathBuf> {
        let h = h
            .strip_prefix("sha256:")
            .filter(|x| x.len() == 64 && x.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| OrchestratorError::InvalidInput("invalid artifact hash".into()))?;
        Ok(self.root.join("artifacts/sha256").join(h))
    }
    fn read<T: DeserializeOwned>(&self, p: &Path) -> Result<T> {
        Ok(serde_json::from_slice(&fs::read(p)?)?)
    }
    fn write_json(&self, p: &Path, v: &impl Serialize) -> Result<()> {
        self.write(p, &serde_json::to_vec_pretty(v)?)
    }
    fn write(&self, p: &Path, b: &[u8]) -> Result<()> {
        let parent = p
            .parent()
            .ok_or_else(|| OrchestratorError::InvalidInput("path has no parent".into()))?;
        fs::create_dir_all(parent)?;
        let t = parent.join(format!(".{}.tmp", Uuid::new_v4()));
        fs::write(&t, b)?;
        fs::rename(t, p)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn budget_triggers_early() {
        let b = ModelBudget {
            context_window: 100_000,
            reserved_output: 10_000,
            safety_margin: 10_000,
            soft_threshold_percent: 75,
        }
        .assess(60_000)
        .unwrap();
        assert_eq!(b.soft_limit, 60_000);
        assert!(b.checkpoint_required)
    }
    #[test]
    fn raw_history_survives_and_head_is_checked() {
        let t = tempfile::tempdir().unwrap();
        let r = ContextRepository::new(t.path()).unwrap();
        let s = ContextScope::Worker(Uuid::new_v4());
        let c = CoreContext {
            specification_version_id: Uuid::new_v4(),
            specification_hash: "hash".into(),
            acceptance_criteria: vec![],
            architecture_decisions: vec![],
            prohibitions: vec![],
            task_boundaries: vec![],
            branch: "work".into(),
            workspace_path: t.path().into(),
        };
        r.save_core(&s, &c).unwrap();
        r.save_working(
            &s,
            &WorkingContext {
                current_task: "task".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let cp = r
            .checkpoint(
                &s,
                CheckpointData {
                    completed_work: vec![],
                    modified_files: vec![],
                    key_decisions: vec![],
                    failed_attempts: vec![],
                    open_questions: vec![],
                    next_steps: vec![],
                    git_status: "clean".into(),
                    workspace_head: "abc".into(),
                },
                b"full history",
            )
            .unwrap();
        assert_eq!(r.read_artifact(&cp.raw_history).unwrap(), b"full history");
        assert!(r
            .rebuild(
                &s,
                c.specification_version_id,
                &c.specification_hash,
                "wrong",
                b"",
                &[]
            )
            .is_err());
        assert!(r
            .rebuild(
                &s,
                c.specification_version_id,
                &c.specification_hash,
                "abc",
                b"diff",
                &[]
            )
            .is_ok())
    }
}
