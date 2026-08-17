use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection, Transaction};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{AgentRunAttempt, AttemptStatus, OrchestratorError, RequirementState, Result};

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
        self.connection
            .lock()
            .expect("database lock poisoned")
            .execute_batch(SCHEMA)?;
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
CREATE TABLE IF NOT EXISTS requirements(id TEXT PRIMARY KEY,project_id TEXT NOT NULL REFERENCES projects(id),title TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('draft','clarifying','ready','planning','executing','integrating','verifying','completed','failed','paused')),confirmed_at TEXT,execution_skip_reason TEXT,updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS clarifications(id TEXT PRIMARY KEY,requirement_id TEXT NOT NULL REFERENCES requirements(id),question TEXT NOT NULL,answer TEXT,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS tasks(id TEXT PRIMARY KEY,requirement_id TEXT NOT NULL REFERENCES requirements(id),title TEXT NOT NULL,status TEXT NOT NULL DEFAULT 'pending');
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
