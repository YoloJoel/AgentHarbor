use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection, Transaction};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    specification_markdown, write_specification, AgentRunAttempt, AttemptStatus, OrchestratorError,
    RequirementSpecification, RequirementState, Result,
};

#[derive(Clone, Debug)]
pub struct FrozenSpecification {
    pub id: Uuid,
    pub requirement_id: Uuid,
    pub version: i64,
    pub hash: String,
    pub document_path: PathBuf,
}

pub trait StateStore {
    fn save<T: Serialize>(&self, name: &str, value: &T) -> Result<()>;
    fn load<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>>;
}

pub struct JsonStateStore {
    directory: PathBuf,
}

impl JsonStateStore {
    pub fn new(directory: impl Into<PathBuf>) -> Result<Self> {
        let directory = directory.into();
        fs::create_dir_all(&directory)?;
        Ok(Self { directory })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.join(format!("{name}.json"))
    }
}

impl StateStore for JsonStateStore {
    fn save<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        let path = self.path(name);
        let temporary = path.with_extension("json.tmp");
        let mut file = File::create(&temporary)?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(temporary, path)?;
        Ok(())
    }

    fn load<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        let path = self.path(name);
        if !Path::new(&path).exists() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_reader(File::open(path)?)?))
    }
}

/// A run that was made durable but whose process disappeared across shutdown.
#[derive(Clone, Debug)]
pub struct RecoveryCandidate {
    pub agent_run_id: Uuid,
    pub interrupted_attempt_id: Uuid,
    pub workspace_id: Uuid,
    pub worktree_path: PathBuf,
    pub checkpoint_context: Option<String>,
}

/// Normalized durable state plus an immutable, ordered audit stream.
pub struct SqliteStateStore {
    connection: Mutex<Connection>,
}

impl SqliteStateStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")?;
        let store = Self {
            connection: Mutex::new(connection),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let connection = self.connection.lock().expect("database lock poisoned");
        connection.execute_batch(SCHEMA)?;
        // The initial prototype pre-dated structured specifications. Keep local databases
        // upgradeable instead of relying on CREATE TABLE IF NOT EXISTS to reshape them.
        ensure_column(
            &connection,
            "requirements",
            "active_specification_id",
            "TEXT",
        )?;
        ensure_column(&connection, "clarifications", "analysis_id", "TEXT")?;
        ensure_column(&connection, "clarifications", "question_key", "TEXT")?;
        ensure_column(&connection, "clarifications", "priority", "TEXT")?;
        ensure_column(&connection, "tasks", "specification_version_id", "TEXT")?;
        ensure_column(&connection, "tasks", "specification_hash", "TEXT")?;
        ensure_column(&connection, "tasks", "specification_change_impact", "TEXT")?;
        Ok(())
    }

    pub fn create_project(&self, id: Uuid, name: &str) -> Result<()> {
        let mut connection = self.connection.lock().expect("database lock poisoned");
        let tx = connection.transaction()?;
        tx.execute(
            "INSERT INTO projects(id,name) VALUES(?1,?2)",
            params![id.to_string(), name],
        )?;
        append_event(
            &tx,
            "project",
            id,
            "project.created",
            &json!({"name": name}),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn create_requirement(&self, id: Uuid, project_id: Uuid, title: &str) -> Result<()> {
        let mut connection = self.connection.lock().expect("database lock poisoned");
        let tx = connection.transaction()?;
        tx.execute(
            "INSERT INTO requirements(id,project_id,title,state) VALUES(?1,?2,?3,'draft')",
            params![id.to_string(), project_id.to_string(), title],
        )?;
        append_event(
            &tx,
            "requirement",
            id,
            "requirement.created",
            &json!({"state":"draft"}),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn confirm_requirement(&self, id: Uuid) -> Result<()> {
        let mut connection = self.connection.lock().expect("database lock poisoned");
        let tx = connection.transaction()?;
        let changed = tx.execute(
            "UPDATE requirements SET confirmed_at=CURRENT_TIMESTAMP WHERE id=?1",
            [id.to_string()],
        )?;
        if changed == 0 {
            return Err(OrchestratorError::InvalidInput(
                "unknown requirement".into(),
            ));
        }
        append_event(&tx, "requirement", id, "requirement.confirmed", &json!({}))?;
        tx.commit()?;
        Ok(())
    }

    /// Creates a new, mutable specification version and its version-controlled Markdown file.
    /// Existing versions are never overwritten.
    pub fn create_specification_version(
        &self,
        requirement_id: Uuid,
        project_root: impl AsRef<Path>,
        specification: &RequirementSpecification,
    ) -> Result<i64> {
        specification.validate()?;
        let mut connection = self.connection.lock().expect("database lock poisoned");
        let tx = connection.transaction()?;
        let version: i64 = tx.query_row(
            "SELECT COALESCE(MAX(version),0)+1 FROM requirement_specifications WHERE requirement_id=?1",
            [requirement_id.to_string()],
            |row| row.get(0),
        )?;
        let previous: Option<String> = tx
            .query_row(
                "SELECT id FROM requirement_specifications WHERE requirement_id=?1 ORDER BY version DESC LIMIT 1",
                [requirement_id.to_string()],
                |row| row.get(0),
            )
            .ok();
        let markdown = specification_markdown(requirement_id, version, specification);
        let relative =
            write_specification(project_root.as_ref(), requirement_id, version, &markdown)?;
        let id = Uuid::new_v4();
        tx.execute(
            "INSERT INTO requirement_specifications(id,requirement_id,version,status,document_path,goal,background,in_scope,out_of_scope,constraints,assumptions,user_scenarios,acceptance_criteria,risks,open_questions,supersedes_id)
             VALUES(?1,?2,?3,'draft',?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            params![id.to_string(), requirement_id.to_string(), version, relative.to_string_lossy(), specification.goal, specification.background,
                json_text(&specification.in_scope)?, json_text(&specification.out_of_scope)?, json_text(&specification.constraints)?,
                json_text(&specification.assumptions)?, json_text(&specification.user_scenarios)?, json_text(&specification.acceptance_criteria)?,
                json_text(&specification.risks)?, json_text(&specification.open_questions)?, previous],
        )?;
        if version > 1 {
            tx.execute(
                "UPDATE tasks SET specification_change_impact='review_required' WHERE requirement_id=?1 AND status!='completed'",
                [requirement_id.to_string()],
            )?;
        }
        append_event(
            &tx,
            "requirement",
            requirement_id,
            "requirement.specification_version_created",
            &json!({"specification_id":id,"version":version,"supersedes_id":previous}),
        )?;
        tx.commit()?;
        Ok(version)
    }

    pub fn record_repository_analysis(
        &self,
        requirement_id: Uuid,
        analysis: &crate::RepositoryAnalysis,
    ) -> Result<Uuid> {
        let mut connection = self.connection.lock().expect("database lock poisoned");
        let tx = connection.transaction()?;
        let id = Uuid::new_v4();
        tx.execute(
            "INSERT INTO repository_analyses(id,requirement_id,known_facts,reasonable_inferences,questions) VALUES(?1,?2,?3,?4,?5)",
            params![id.to_string(), requirement_id.to_string(), json_text(&analysis.known_facts)?, json_text(&analysis.reasonable_inferences)?, json_text(&analysis.questions_for_user)?],
        )?;
        for question in &analysis.questions_for_user {
            tx.execute(
                "INSERT INTO clarifications(id,requirement_id,analysis_id,question_key,priority,question) VALUES(?1,?2,?3,?4,?5,?6)",
                params![Uuid::new_v4().to_string(), requirement_id.to_string(), id.to_string(), question.key, format!("{:?}", question.priority).to_lowercase(), question.question],
            )?;
        }
        append_event(
            &tx,
            "requirement",
            requirement_id,
            "requirement.repository_analyzed",
            &json!({"analysis_id":id,"question_count":analysis.questions_for_user.len()}),
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Freezes the confirmed version, hashes the exact Markdown artifact, and makes it the
    /// requirement's sole active version. Plans, tasks, and acceptance records bind to this id/hash.
    pub fn freeze_specification(
        &self,
        requirement_id: Uuid,
        version: i64,
        project_root: impl AsRef<Path>,
    ) -> Result<FrozenSpecification> {
        let mut connection = self.connection.lock().expect("database lock poisoned");
        let tx = connection.transaction()?;
        let confirmed: Option<String> = tx.query_row(
            "SELECT confirmed_at FROM requirements WHERE id=?1",
            [requirement_id.to_string()],
            |row| row.get(0),
        )?;
        if confirmed.is_none() {
            return Err(OrchestratorError::InvalidInput(
                "user confirmation is required before freezing a specification".into(),
            ));
        }
        let (id, status, document_path): (String, String, String) = tx.query_row(
            "SELECT id,status,document_path FROM requirement_specifications WHERE requirement_id=?1 AND version=?2",
            params![requirement_id.to_string(), version],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if status != "draft" {
            return Err(OrchestratorError::InvalidInput(
                "only a draft specification can be frozen".into(),
            ));
        }
        let path = project_root.as_ref().join(&document_path);
        let draft = fs::read_to_string(&path)?;
        let frozen = draft.replacen("- Status: draft", "- Status: frozen", 1);
        fs::write(&path, frozen.as_bytes())?;
        let hash = format!("sha256:{:x}", Sha256::digest(frozen.as_bytes()));
        tx.execute("UPDATE requirement_specifications SET status='superseded' WHERE requirement_id=?1 AND status='frozen'", [requirement_id.to_string()])?;
        tx.execute("UPDATE requirement_specifications SET status='frozen',spec_hash=?3,frozen_at=CURRENT_TIMESTAMP WHERE requirement_id=?1 AND version=?2",
            params![requirement_id.to_string(), version, hash])?;
        tx.execute("UPDATE requirements SET active_specification_id=?2,updated_at=CURRENT_TIMESTAMP WHERE id=?1",
            params![requirement_id.to_string(), id])?;
        let parsed_id = parse_uuid(&id)?;
        append_event(
            &tx,
            "requirement",
            requirement_id,
            "requirement.specification_frozen",
            &json!({"specification_id":parsed_id,"version":version,"hash":hash}),
        )?;
        tx.commit()?;
        Ok(FrozenSpecification {
            id: parsed_id,
            requirement_id,
            version,
            hash,
            document_path: PathBuf::from(document_path),
        })
    }

    pub fn create_task_for_specification(
        &self,
        id: Uuid,
        requirement_id: Uuid,
        title: &str,
    ) -> Result<()> {
        let mut connection = self.connection.lock().expect("database lock poisoned");
        let tx = connection.transaction()?;
        let (spec_id, hash): (String, String) = tx.query_row(
            "SELECT s.id,s.spec_hash FROM requirements r JOIN requirement_specifications s ON s.id=r.active_specification_id WHERE r.id=?1 AND s.status='frozen'",
            [requirement_id.to_string()], |row| Ok((row.get(0)?, row.get(1)?)))?;
        tx.execute("INSERT INTO tasks(id,requirement_id,title,specification_version_id,specification_hash) VALUES(?1,?2,?3,?4,?5)",
            params![id.to_string(), requirement_id.to_string(), title, spec_id, hash])?;
        append_event(
            &tx,
            "task",
            id,
            "task.created",
            &json!({"specification_version_id":spec_id,"specification_hash":hash}),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn record_execution_plan(&self, id: Uuid, requirement_id: Uuid, body: &str) -> Result<()> {
        self.insert_spec_reference("execution_plans", id, requirement_id, "body", body)
    }

    pub fn record_final_acceptance(
        &self,
        id: Uuid,
        requirement_id: Uuid,
        evidence: &str,
    ) -> Result<()> {
        self.insert_spec_reference(
            "acceptance_records",
            id,
            requirement_id,
            "evidence",
            evidence,
        )
    }

    fn insert_spec_reference(
        &self,
        table: &str,
        id: Uuid,
        requirement_id: Uuid,
        field: &str,
        value: &str,
    ) -> Result<()> {
        let connection = self.connection.lock().expect("database lock poisoned");
        let (spec_id, hash): (String, String) = connection.query_row(
            "SELECT s.id,s.spec_hash FROM requirements r JOIN requirement_specifications s ON s.id=r.active_specification_id WHERE r.id=?1 AND s.status='frozen'",
            [requirement_id.to_string()], |row| Ok((row.get(0)?, row.get(1)?)))?;
        // Table and field are private constants chosen by the two callers above.
        let sql = format!("INSERT INTO {table}(id,requirement_id,{field},specification_version_id,specification_hash) VALUES(?1,?2,?3,?4,?5)");
        connection.execute(
            &sql,
            params![
                id.to_string(),
                requirement_id.to_string(),
                value,
                spec_id,
                hash
            ],
        )?;
        Ok(())
    }

    /// Atomically changes the projection and records why it changed.
    pub fn transition_requirement(
        &self,
        id: Uuid,
        next: RequirementState,
        skip_reason: Option<&str>,
    ) -> Result<()> {
        let mut connection = self.connection.lock().expect("database lock poisoned");
        let tx = connection.transaction()?;
        let (current, confirmed): (String, Option<String>) = tx.query_row(
            "SELECT state,confirmed_at FROM requirements WHERE id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let current = parse_requirement_state(&current)?;
        let explicit_skip = skip_reason.is_some_and(|reason| !reason.trim().is_empty());
        if !current.permits(&next) && !(next == RequirementState::Executing && explicit_skip) {
            return Err(OrchestratorError::InvalidInput(format!(
                "invalid requirement transition: {} -> {}",
                current.as_str(),
                next.as_str()
            )));
        }
        if next == RequirementState::Executing && confirmed.is_none() && !explicit_skip {
            return Err(OrchestratorError::InvalidInput(
                "unconfirmed requirement requires an explicit execution skip reason".into(),
            ));
        }
        tx.execute("UPDATE requirements SET state=?2, execution_skip_reason=COALESCE(?3,execution_skip_reason), updated_at=CURRENT_TIMESTAMP WHERE id=?1",
            params![id.to_string(), next.as_str(), skip_reason])?;
        append_event(
            &tx,
            "requirement",
            id,
            "requirement.state_changed",
            &json!({"from":current.as_str(), "to":next.as_str(), "skip_reason":skip_reason}),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Creates a new process instance without changing the logical run identity.
    pub fn start_attempt(&self, agent_run_id: Uuid, pid: Option<u32>) -> Result<AgentRunAttempt> {
        let mut connection = self.connection.lock().expect("database lock poisoned");
        let tx = connection.transaction()?;
        let number: i64 = tx.query_row("SELECT COALESCE(MAX(attempt_number),0)+1 FROM agent_run_attempts WHERE agent_run_id=?1",
            [agent_run_id.to_string()], |r| r.get(0))?;
        let id = Uuid::new_v4();
        tx.execute("INSERT INTO agent_run_attempts(id,agent_run_id,attempt_number,pid,status) VALUES(?1,?2,?3,?4,'running')",
            params![id.to_string(), agent_run_id.to_string(), number, pid])?;
        append_event(
            &tx,
            "agent_run",
            agent_run_id,
            "agent_run.attempt_started",
            &json!({"attempt_id":id,"attempt_number":number,"pid":pid}),
        )?;
        tx.commit()?;
        Ok(AgentRunAttempt {
            id,
            agent_run_id,
            attempt_number: number,
            pid,
            status: AttemptStatus::Running,
        })
    }

    pub fn record_event(
        &self,
        entity_type: &str,
        entity_id: Uuid,
        event_type: &str,
        payload: &serde_json::Value,
    ) -> Result<i64> {
        let mut connection = self.connection.lock().expect("database lock poisoned");
        let tx = connection.transaction()?;
        let sequence = append_event(&tx, entity_type, entity_id, event_type, payload)?;
        tx.commit()?;
        Ok(sequence)
    }

    /// Startup reconciliation. The caller supplies OS/container-specific liveness detection.
    /// Every missing process is marked interrupted in the same transaction as its audit event.
    pub fn reconcile_running_attempts(
        &self,
        is_alive: impl Fn(u32) -> bool,
    ) -> Result<Vec<RecoveryCandidate>> {
        let mut connection = self.connection.lock().expect("database lock poisoned");
        let tx = connection.transaction()?;
        let mut statement = tx.prepare("SELECT a.id,a.agent_run_id,a.pid,r.workspace_id,w.worktree_path,
            (SELECT context FROM checkpoints c WHERE c.agent_run_id=a.agent_run_id ORDER BY sequence DESC LIMIT 1)
            FROM agent_run_attempts a JOIN agent_runs r ON r.id=a.agent_run_id JOIN workspaces w ON w.id=r.workspace_id
            WHERE a.status IN ('starting','running')")?;
        let rows = statement.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<u32>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?;
        let mut candidates = Vec::new();
        for row in rows {
            let (attempt, run, pid, workspace, path, context) = row?;
            if pid.is_none_or(|pid| !is_alive(pid)) {
                tx.execute("UPDATE agent_run_attempts SET status='interrupted',ended_at=CURRENT_TIMESTAMP WHERE id=?1", [&attempt])?;
                let attempt_id = parse_uuid(&attempt)?;
                let agent_run_id = parse_uuid(&run)?;
                append_event(
                    &tx,
                    "agent_run",
                    agent_run_id,
                    "agent_run.interrupted",
                    &json!({"attempt_id":attempt_id,"reason":"process_missing"}),
                )?;
                candidates.push(RecoveryCandidate {
                    agent_run_id,
                    interrupted_attempt_id: attempt_id,
                    workspace_id: parse_uuid(&workspace)?,
                    worktree_path: PathBuf::from(path),
                    checkpoint_context: context,
                });
            }
        }
        drop(statement);
        tx.commit()?;
        Ok(candidates)
    }
}

fn append_event(
    tx: &Transaction<'_>,
    entity_type: &str,
    entity_id: Uuid,
    event_type: &str,
    payload: &serde_json::Value,
) -> Result<i64> {
    tx.execute(
        "INSERT INTO events(entity_type,entity_id,event_type,payload) VALUES(?1,?2,?3,?4)",
        params![
            entity_type,
            entity_id.to_string(),
            event_type,
            serde_json::to_string(payload)?
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

fn parse_uuid(value: &str) -> Result<Uuid> {
    Uuid::parse_str(value)
        .map_err(|_| OrchestratorError::InvalidInput("invalid persisted UUID".into()))
}
fn json_text<T: Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}
fn ensure_column(connection: &Connection, table: &str, column: &str, kind: &str) -> Result<()> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = statement.query_map([], |row| row.get::<_, String>(1))?;
    for name in names {
        if name? == column {
            return Ok(());
        }
    }
    // All identifiers and types are private constants from migrate(), never external input.
    connection.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {kind}"))?;
    Ok(())
}
fn parse_requirement_state(value: &str) -> Result<RequirementState> {
    Ok(match value {
        "draft" => RequirementState::Draft,
        "clarifying" => RequirementState::Clarifying,
        "ready" => RequirementState::Ready,
        "planning" => RequirementState::Planning,
        "executing" => RequirementState::Executing,
        "integrating" => RequirementState::Integrating,
        "verifying" => RequirementState::Verifying,
        "completed" => RequirementState::Completed,
        "failed" => RequirementState::Failed,
        "paused" => RequirementState::Paused,
        _ => {
            return Err(OrchestratorError::InvalidInput(
                "invalid persisted requirement state".into(),
            ))
        }
    })
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS projects(id TEXT PRIMARY KEY,name TEXT NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS requirements(id TEXT PRIMARY KEY,project_id TEXT NOT NULL REFERENCES projects(id),title TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('draft','clarifying','ready','planning','executing','integrating','verifying','completed','failed','paused')),confirmed_at TEXT,execution_skip_reason TEXT,active_specification_id TEXT,updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS repository_analyses(id TEXT PRIMARY KEY,requirement_id TEXT NOT NULL REFERENCES requirements(id),known_facts TEXT NOT NULL,reasonable_inferences TEXT NOT NULL,questions TEXT NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS clarifications(id TEXT PRIMARY KEY,requirement_id TEXT NOT NULL REFERENCES requirements(id),analysis_id TEXT REFERENCES repository_analyses(id),question_key TEXT,priority TEXT,question TEXT NOT NULL,answer TEXT,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS requirement_specifications(id TEXT PRIMARY KEY,requirement_id TEXT NOT NULL REFERENCES requirements(id),version INTEGER NOT NULL,status TEXT NOT NULL CHECK(status IN ('draft','frozen','superseded')),document_path TEXT NOT NULL,goal TEXT NOT NULL,background TEXT NOT NULL,in_scope TEXT NOT NULL,out_of_scope TEXT NOT NULL,constraints TEXT NOT NULL,assumptions TEXT NOT NULL,user_scenarios TEXT NOT NULL,acceptance_criteria TEXT NOT NULL,risks TEXT NOT NULL,open_questions TEXT NOT NULL,spec_hash TEXT,frozen_at TEXT,supersedes_id TEXT REFERENCES requirement_specifications(id),created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,UNIQUE(requirement_id,version));
CREATE UNIQUE INDEX IF NOT EXISTS one_frozen_specification_per_requirement ON requirement_specifications(requirement_id) WHERE status='frozen';
CREATE TABLE IF NOT EXISTS tasks(id TEXT PRIMARY KEY,requirement_id TEXT NOT NULL REFERENCES requirements(id),title TEXT NOT NULL,status TEXT NOT NULL DEFAULT 'pending',specification_version_id TEXT NOT NULL REFERENCES requirement_specifications(id),specification_hash TEXT NOT NULL,specification_change_impact TEXT);
CREATE TABLE IF NOT EXISTS execution_plans(id TEXT PRIMARY KEY,requirement_id TEXT NOT NULL REFERENCES requirements(id),body TEXT NOT NULL,specification_version_id TEXT NOT NULL REFERENCES requirement_specifications(id),specification_hash TEXT NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS acceptance_records(id TEXT PRIMARY KEY,requirement_id TEXT NOT NULL REFERENCES requirements(id),evidence TEXT NOT NULL,specification_version_id TEXT NOT NULL REFERENCES requirement_specifications(id),specification_hash TEXT NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS agent_roles(id TEXT PRIMARY KEY,project_id TEXT NOT NULL REFERENCES projects(id),name TEXT NOT NULL,instructions TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS workspaces(id TEXT PRIMARY KEY,project_id TEXT NOT NULL REFERENCES projects(id),repository_path TEXT NOT NULL,worktree_path TEXT NOT NULL UNIQUE);
CREATE TABLE IF NOT EXISTS agent_runs(id TEXT PRIMARY KEY,agent_role_id TEXT NOT NULL REFERENCES agent_roles(id),task_id TEXT NOT NULL REFERENCES tasks(id),workspace_id TEXT NOT NULL REFERENCES workspaces(id),recovery_policy TEXT NOT NULL DEFAULT 'resume');
CREATE TABLE IF NOT EXISTS agent_run_attempts(id TEXT PRIMARY KEY,agent_run_id TEXT NOT NULL REFERENCES agent_runs(id),attempt_number INTEGER NOT NULL,pid INTEGER,status TEXT NOT NULL CHECK(status IN ('starting','running','completed','failed','interrupted')),started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,ended_at TEXT,UNIQUE(agent_run_id,attempt_number));
CREATE TABLE IF NOT EXISTS messages(id TEXT PRIMARY KEY,agent_run_id TEXT NOT NULL REFERENCES agent_runs(id),direction TEXT NOT NULL,body TEXT NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS artifacts(id TEXT PRIMARY KEY,task_id TEXT NOT NULL REFERENCES tasks(id),workspace_id TEXT NOT NULL REFERENCES workspaces(id),path TEXT NOT NULL,kind TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS approvals(id TEXT PRIMARY KEY,agent_run_id TEXT NOT NULL REFERENCES agent_runs(id),operation TEXT NOT NULL,decision TEXT NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS checkpoints(id TEXT PRIMARY KEY,agent_run_id TEXT NOT NULL REFERENCES agent_runs(id),attempt_id TEXT NOT NULL REFERENCES agent_run_attempts(id),sequence INTEGER NOT NULL,context TEXT NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,UNIQUE(agent_run_id,sequence));
CREATE TABLE IF NOT EXISTS events(sequence INTEGER PRIMARY KEY AUTOINCREMENT,entity_type TEXT NOT NULL,entity_id TEXT NOT NULL,event_type TEXT NOT NULL,payload TEXT NOT NULL,occurred_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TRIGGER IF NOT EXISTS events_no_update BEFORE UPDATE ON events BEGIN SELECT RAISE(ABORT,'events are append-only'); END;
CREATE TRIGGER IF NOT EXISTS events_no_delete BEFORE DELETE ON events BEGIN SELECT RAISE(ABORT,'events are append-only'); END;
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_spec(goal: &str) -> RequirementSpecification {
        RequirementSpecification {
            goal: goal.into(),
            background: "Operators need an auditable delivery contract.".into(),
            in_scope: vec!["Persist a structured specification.".into()],
            out_of_scope: vec!["Agent implementation details.".into()],
            constraints: vec!["Do not mutate frozen versions.".into()],
            assumptions: vec!["The project uses Git.".into()],
            user_scenarios: vec!["A lead confirms a specification before work starts.".into()],
            acceptance_criteria: vec!["Every task references a frozen hash.".into()],
            risks: vec!["Stale tasks may target an older version.".into()],
            open_questions: vec![],
        }
    }

    #[test]
    fn freezes_versions_and_marks_tasks_impacted_by_change() {
        let directory = tempfile::tempdir().unwrap();
        let store = SqliteStateStore::open(directory.path().join("state.db")).unwrap();
        let project = Uuid::new_v4();
        let requirement = Uuid::new_v4();
        store.create_project(project, "harbor").unwrap();
        store
            .create_requirement(requirement, project, "version specifications")
            .unwrap();
        let first = store
            .create_specification_version(
                requirement,
                directory.path(),
                &sample_spec("Ship safely"),
            )
            .unwrap();
        assert_eq!(first, 1);
        assert!(store
            .freeze_specification(requirement, first, directory.path())
            .is_err());
        store.confirm_requirement(requirement).unwrap();
        let frozen = store
            .freeze_specification(requirement, first, directory.path())
            .unwrap();
        assert!(frozen.hash.starts_with("sha256:"));
        assert!(directory.path().join(&frozen.document_path).exists());

        let task = Uuid::new_v4();
        store
            .create_task_for_specification(task, requirement, "implement")
            .unwrap();
        let second = store
            .create_specification_version(
                requirement,
                directory.path(),
                &sample_spec("Ship more safely"),
            )
            .unwrap();
        assert_eq!(second, 2);
        let impact: String = store
            .connection
            .lock()
            .unwrap()
            .query_row(
                "SELECT specification_change_impact FROM tasks WHERE id=?1",
                [task.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(impact, "review_required");
    }

    #[test]
    fn analysis_questions_put_architecture_before_acceptance() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("Cargo.toml"), "[workspace]").unwrap();
        fs::write(directory.path().join("package.json"), "{}").unwrap();
        let analysis = crate::analyze_repository(directory.path()).unwrap();
        assert!(analysis.known_facts.len() >= 2);
        assert_eq!(
            analysis.questions_for_user[0].priority,
            crate::QuestionPriority::Architecture
        );
        assert_eq!(
            analysis.questions_for_user[2].priority,
            crate::QuestionPriority::Acceptance
        );
    }
}
