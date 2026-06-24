use std::collections::HashMap;
use std::path::PathBuf;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use crate::tool_trace::{ToolTraceEvent, TraceEventType, TraceStatus};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceHistoryState {
    pub active_project_id: Option<String>,
    pub current_workspace_task_id: Option<String>,
    pub tasks_by_id: HashMap<String, AgentTaskRecord>,
    pub task_ids_by_project_id: HashMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTaskRecord {
    pub id: String,
    pub project_id: String,
    pub prompt: String,
    pub messages: Vec<ChatMessageRecord>,
    pub trace_events: Vec<ToolTraceEvent>,
    pub status: String,
    #[serde(default = "default_messages_loaded")]
    pub messages_loaded: bool,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessageRecord {
    pub id: String,
    pub task_id: String,
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_links: Option<Vec<CodeLinkRecord>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<MessageAttachmentRecord>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_events: Option<Vec<ToolTraceEvent>>,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeLinkRecord {
    pub raw_link: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageAttachmentRecord {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub mime_type: String,
    pub data_url: String,
}

pub struct HistoryStore {
    conn: Connection,
}

impl HistoryStore {
    pub fn load(path: PathBuf) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "SQLite data directory create failed {}: {error}",
                    parent.to_string_lossy()
                )
            })?;
        }
        let conn = Connection::open(&path)
            .map_err(|error| format!("SQLite open failed {}: {error}", path.to_string_lossy()))?;
        let store = Self { conn };
        store.init_schema()?;
        store.fail_abandoned_running_runs()?;
        Ok(store)
    }

    pub fn load_workspace_history(&self) -> Result<WorkspaceHistoryState, String> {
        let active_project_id = self.meta_value("active_project_id")?;
        let current_workspace_task_id = self.meta_value("current_workspace_task_id")?;
        let mut tasks_by_id = HashMap::new();
        let mut task_ids_by_project_id: HashMap<String, Vec<String>> = HashMap::new();

        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, project_id, prompt, status, created_at, updated_at
                 FROM conversation_sessions
                 ORDER BY project_id ASC, position ASC, updated_at ASC, id ASC",
            )
            .map_err(sql_error)?;
        let sessions = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .map_err(sql_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql_error)?;

        for (id, project_id, prompt, status, created_at, updated_at) in sessions {
            task_ids_by_project_id
                .entry(project_id.clone())
                .or_default()
                .push(id.clone());
            tasks_by_id.insert(
                id.clone(),
                AgentTaskRecord {
                    id,
                    project_id,
                    prompt,
                    messages: Vec::new(),
                    trace_events: Vec::new(),
                    status,
                    messages_loaded: false,
                    created_at,
                    updated_at,
                },
            );
        }

        Ok(WorkspaceHistoryState {
            active_project_id,
            current_workspace_task_id,
            tasks_by_id,
            task_ids_by_project_id,
        })
    }

    pub fn load_workspace_session(&self, session_id: &str) -> Result<AgentTaskRecord, String> {
        let (id, project_id, prompt, status, created_at, updated_at) = self
            .conn
            .query_row(
                "SELECT id, project_id, prompt, status, created_at, updated_at
                 FROM conversation_sessions
                 WHERE id = ?1",
                params![session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(sql_error)?
            .ok_or_else(|| format!("Conversation session not found: {session_id}"))?;
        let messages = self.load_messages(&id)?;
        let trace_events = messages
            .iter()
            .rev()
            .find_map(|message| {
                message
                    .trace_events
                    .as_ref()
                    .filter(|events| !events.is_empty())
                    .cloned()
            })
            .unwrap_or_default();

        Ok(AgentTaskRecord {
            id,
            project_id,
            prompt,
            messages,
            trace_events,
            status,
            messages_loaded: true,
            created_at,
            updated_at,
        })
    }

    pub fn save_workspace_history(
        &mut self,
        state: &WorkspaceHistoryState,
        persist_embedded_traces: bool,
    ) -> Result<(), String> {
        self.set_meta_value("active_project_id", state.active_project_id.as_deref())?;
        self.set_meta_value(
            "current_workspace_task_id",
            state.current_workspace_task_id.as_deref(),
        )?;

        let positions = session_positions(&state.task_ids_by_project_id);
        for task in state.tasks_by_id.values() {
            self.upsert_session(task, positions.get(&task.id).copied().unwrap_or(0))?;
            if task.messages_loaded {
                self.replace_messages(&task.id, &task.messages)?;
            }
            if persist_embedded_traces {
                for event in &task.trace_events {
                    self.insert_trace_event(&task.id, event)?;
                }
                for message in &task.messages {
                    if let Some(events) = &message.trace_events {
                        for event in events {
                            self.insert_trace_event(&task.id, event)?;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    pub fn save_workspace_session(
        &mut self,
        task: &AgentTaskRecord,
        position: i64,
    ) -> Result<(), String> {
        self.upsert_session(task, position)?;
        if task.messages_loaded {
            self.replace_messages(&task.id, &task.messages)?;
        }
        Ok(())
    }

    pub fn save_workspace_selection(
        &self,
        active_project_id: Option<&str>,
        current_workspace_task_id: Option<&str>,
    ) -> Result<(), String> {
        self.set_meta_value("active_project_id", active_project_id)?;
        self.set_meta_value("current_workspace_task_id", current_workspace_task_id)?;
        Ok(())
    }

    pub fn delete_workspace_sessions(&mut self, session_ids: &[String]) -> Result<(), String> {
        for session_id in session_ids {
            self.conn
                .execute(
                    "DELETE FROM trace_events WHERE session_id = ?1",
                    params![session_id],
                )
                .map_err(sql_error)?;
            self.conn
                .execute(
                    "DELETE FROM agent_runs WHERE session_id = ?1",
                    params![session_id],
                )
                .map_err(sql_error)?;
            self.conn
                .execute(
                    "DELETE FROM conversation_messages WHERE session_id = ?1",
                    params![session_id],
                )
                .map_err(sql_error)?;
            self.conn
                .execute(
                    "DELETE FROM conversation_sessions WHERE id = ?1",
                    params![session_id],
                )
                .map_err(sql_error)?;
        }
        Ok(())
    }

    pub fn insert_trace_event(
        &mut self,
        session_id: &str,
        event: &ToolTraceEvent,
    ) -> Result<(), String> {
        self.upsert_agent_run(session_id, event)?;
        self.conn
            .execute(
                "INSERT OR REPLACE INTO trace_events (
                    id, run_id, session_id, step_index, event_type, tool_name, title,
                    input_json, output_json, output_summary, started_at, ended_at,
                    duration_ms, status
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    event.id.as_str(),
                    event.task_id.as_str(),
                    session_id,
                    i64::from(event.step_index),
                    enum_text(&event.event_type)?,
                    event.tool_name.as_deref(),
                    event.title.as_str(),
                    optional_json(&event.input)?,
                    optional_json(&event.output)?,
                    event.output_summary.as_deref(),
                    event.started_at.as_str(),
                    event.ended_at.as_deref(),
                    event.duration_ms.map(|value| value as i64),
                    enum_text(&event.status)?,
                ],
            )
            .map_err(sql_error)?;
        Ok(())
    }

    pub fn list_trace_events(&self, run_id: &str) -> Result<Vec<ToolTraceEvent>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, run_id, step_index, event_type, tool_name, title,
                        input_json, output_json, output_summary, started_at,
                        ended_at, duration_ms, status
                 FROM trace_events
                 WHERE run_id = ?1
                 ORDER BY step_index ASC, started_at ASC, id ASC",
            )
            .map_err(sql_error)?;
        let rows = stmt
            .query_map(params![run_id], |row| {
                let event_type_text: String = row.get(3)?;
                let status_text: String = row.get(12)?;
                Ok(TraceEventRow {
                    id: row.get(0)?,
                    task_id: row.get(1)?,
                    step_index: row.get::<_, i64>(2)? as u32,
                    event_type_text,
                    tool_name: row.get(4)?,
                    title: row.get(5)?,
                    input_json: row.get(6)?,
                    output_json: row.get(7)?,
                    output_summary: row.get(8)?,
                    started_at: row.get(9)?,
                    ended_at: row.get(10)?,
                    duration_ms: row.get::<_, Option<i64>>(11)?.map(|value| value as u64),
                    status_text,
                })
            })
            .map_err(sql_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql_error)?;

        rows.into_iter()
            .map(TraceEventRow::into_event)
            .collect::<Result<Vec<_>, _>>()
    }

    pub fn update_agent_run_metadata(
        &mut self,
        run_id: &str,
        provider_id: Option<&str>,
        model_id: Option<&str>,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE agent_runs
                 SET provider_id = COALESCE(?2, provider_id),
                     model_id = COALESCE(?3, model_id)
                 WHERE run_id = ?1",
                params![run_id, provider_id, model_id],
            )
            .map_err(sql_error)?;
        Ok(())
    }

    fn fail_abandoned_running_runs(&self) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let runs = {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT run_id, session_id
                     FROM agent_runs
                     WHERE status = 'running'",
                )
                .map_err(sql_error)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(sql_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_error)?;
            rows
        };

        for (run_id, session_id) in runs {
            let next_step_index = self
                .conn
                .query_row(
                    "SELECT COALESCE(MAX(step_index), 0) + 1
                     FROM trace_events
                     WHERE run_id = ?1",
                    params![run_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(sql_error)?;
            let summary = "Agent run was abandoned before completion. The previous CodeForge process exited before the model request returned.";
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO trace_events (
                        id, run_id, session_id, step_index, event_type, tool_name, title,
                        input_json, output_json, output_summary, started_at, ended_at,
                        duration_ms, status
                    ) VALUES (?1, ?2, ?3, ?4, 'error', NULL, 'agent_run_abandoned',
                        NULL, ?5, ?6, ?7, ?7, 0, 'failed')",
                    params![
                        uuid::Uuid::new_v4().to_string(),
                        run_id.as_str(),
                        session_id.as_str(),
                        next_step_index,
                        serde_json::json!({ "error": summary }).to_string(),
                        summary,
                        now.as_str(),
                    ],
                )
                .map_err(sql_error)?;
            self.conn
                .execute(
                    "UPDATE agent_runs
                     SET status = 'failed',
                         ended_at = COALESCE(ended_at, ?2),
                         final_summary = ?3
                     WHERE run_id = ?1 AND status = 'running'",
                    params![run_id.as_str(), now.as_str(), summary],
                )
                .map_err(sql_error)?;
            self.conn
                .execute(
                    "UPDATE conversation_sessions
                     SET status = 'failed',
                         updated_at = ?2
                     WHERE id = ?1 AND status = 'running'",
                    params![session_id.as_str(), now.as_str()],
                )
                .map_err(sql_error)?;
            self.conn
                .execute(
                    "UPDATE conversation_messages
                     SET status = 'failed'
                     WHERE task_id = ?1 AND status = 'running'",
                    params![run_id.as_str()],
                )
                .map_err(sql_error)?;
        }

        Ok(())
    }

    fn init_schema(&self) -> Result<(), String> {
        self.conn
            .execute_batch(
                r#"
                PRAGMA foreign_keys = ON;

                CREATE TABLE IF NOT EXISTS app_state (
                    key TEXT PRIMARY KEY,
                    value TEXT
                );

                CREATE TABLE IF NOT EXISTS conversation_sessions (
                    id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL,
                    prompt TEXT NOT NULL,
                    status TEXT NOT NULL,
                    position INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );

                CREATE TABLE IF NOT EXISTS conversation_messages (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    task_id TEXT NOT NULL,
                    role TEXT NOT NULL,
                    content TEXT NOT NULL,
                    status TEXT,
                    code_links_json TEXT,
                    attachments_json TEXT,
                    created_at TEXT NOT NULL,
                    sort_index INTEGER NOT NULL,
                    FOREIGN KEY(session_id) REFERENCES conversation_sessions(id) ON DELETE CASCADE
                );

                CREATE TABLE IF NOT EXISTS agent_runs (
                    run_id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    provider_id TEXT,
                    model_id TEXT,
                    status TEXT NOT NULL,
                    started_at TEXT NOT NULL,
                    ended_at TEXT,
                    final_summary TEXT
                );

                CREATE TABLE IF NOT EXISTS trace_events (
                    id TEXT PRIMARY KEY,
                    run_id TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    step_index INTEGER NOT NULL,
                    event_type TEXT NOT NULL,
                    tool_name TEXT,
                    title TEXT NOT NULL,
                    input_json TEXT,
                    output_json TEXT,
                    output_summary TEXT,
                    started_at TEXT NOT NULL,
                    ended_at TEXT,
                    duration_ms INTEGER,
                    status TEXT NOT NULL,
                    FOREIGN KEY(run_id) REFERENCES agent_runs(run_id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_conversation_sessions_project
                    ON conversation_sessions(project_id, position);
                CREATE INDEX IF NOT EXISTS idx_conversation_messages_session
                    ON conversation_messages(session_id, sort_index);
                CREATE INDEX IF NOT EXISTS idx_agent_runs_session
                    ON agent_runs(session_id, started_at);
                CREATE INDEX IF NOT EXISTS idx_trace_events_run
                    ON trace_events(run_id, step_index);
                "#,
            )
            .map_err(sql_error)
    }

    fn meta_value(&self, key: &str) -> Result<Option<String>, String> {
        self.conn
            .query_row(
                "SELECT value FROM app_state WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_error)
    }

    fn set_meta_value(&self, key: &str, value: Option<&str>) -> Result<(), String> {
        match value {
            Some(value) => {
                self.conn
                    .execute(
                        "INSERT OR REPLACE INTO app_state (key, value) VALUES (?1, ?2)",
                        params![key, value],
                    )
                    .map_err(sql_error)?;
            }
            None => {
                self.conn
                    .execute("DELETE FROM app_state WHERE key = ?1", params![key])
                    .map_err(sql_error)?;
            }
        }
        Ok(())
    }

    fn upsert_session(&self, task: &AgentTaskRecord, position: i64) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let created_at = if !task.created_at.is_empty() {
            task.created_at.as_str()
        } else {
            task.messages
                .first()
                .map(|message| message.created_at.as_str())
                .unwrap_or(now.as_str())
        };
        let updated_at = if !task.updated_at.is_empty() {
            task.updated_at.as_str()
        } else {
            task.messages
                .last()
                .map(|message| message.created_at.as_str())
                .unwrap_or(now.as_str())
        };
        self.conn
            .execute(
                "INSERT INTO conversation_sessions (
                    id, project_id, prompt, status, position, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET
                    project_id = excluded.project_id,
                    prompt = excluded.prompt,
                    status = excluded.status,
                    position = excluded.position,
                    updated_at = excluded.updated_at",
                params![
                    task.id.as_str(),
                    task.project_id.as_str(),
                    task.prompt.as_str(),
                    task.status.as_str(),
                    position,
                    created_at,
                    updated_at,
                ],
            )
            .map_err(sql_error)?;
        Ok(())
    }

    fn replace_messages(
        &self,
        session_id: &str,
        messages: &[ChatMessageRecord],
    ) -> Result<(), String> {
        self.conn
            .execute(
                "DELETE FROM conversation_messages WHERE session_id = ?1",
                params![session_id],
            )
            .map_err(sql_error)?;
        for (index, message) in messages.iter().enumerate() {
            self.conn
                .execute(
                    "INSERT INTO conversation_messages (
                        id, session_id, task_id, role, content, status,
                        code_links_json, attachments_json, created_at, sort_index
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        message.id.as_str(),
                        session_id,
                        message.task_id.as_str(),
                        message.role.as_str(),
                        message.content.as_str(),
                        message.status.as_deref(),
                        optional_json(&message.code_links)?,
                        optional_json(&message.attachments)?,
                        message.created_at.as_str(),
                        index as i64,
                    ],
                )
                .map_err(sql_error)?;
        }
        Ok(())
    }

    fn load_messages(&self, session_id: &str) -> Result<Vec<ChatMessageRecord>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, task_id, role, content, status, code_links_json,
                        attachments_json, created_at
                 FROM conversation_messages
                 WHERE session_id = ?1
                 ORDER BY sort_index ASC, created_at ASC, id ASC",
            )
            .map_err(sql_error)?;
        let rows = stmt
            .query_map(params![session_id], |row| {
                Ok(MessageRow {
                    id: row.get(0)?,
                    task_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    status: row.get(4)?,
                    code_links_json: row.get(5)?,
                    attachments_json: row.get(6)?,
                    created_at: row.get(7)?,
                })
            })
            .map_err(sql_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql_error)?;

        let mut traces_by_task_id: HashMap<String, Vec<ToolTraceEvent>> = HashMap::new();
        rows.into_iter()
            .map(|row| {
                let trace_events = if let Some(trace_events) = traces_by_task_id.get(&row.task_id) {
                    trace_events.clone()
                } else {
                    let trace_events = self.list_trace_events(&row.task_id)?;
                    traces_by_task_id.insert(row.task_id.clone(), trace_events.clone());
                    trace_events
                };
                Ok(ChatMessageRecord {
                    id: row.id,
                    task_id: row.task_id,
                    role: row.role,
                    content: row.content,
                    status: row.status,
                    code_links: decode_optional_json(row.code_links_json)?,
                    attachments: decode_optional_json(row.attachments_json)?,
                    trace_events: (!trace_events.is_empty()).then_some(trace_events),
                    created_at: row.created_at,
                })
            })
            .collect()
    }

    fn upsert_agent_run(&self, session_id: &str, event: &ToolTraceEvent) -> Result<(), String> {
        let existing_started_at = self
            .conn
            .query_row(
                "SELECT started_at FROM agent_runs WHERE run_id = ?1",
                params![event.task_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(sql_error)?;
        let status = match event.status {
            TraceStatus::Failed => "failed",
            TraceStatus::Warning => "warning",
            TraceStatus::Running => "running",
            TraceStatus::Success => {
                if matches!(event.event_type, TraceEventType::FinalResponse) {
                    "completed"
                } else {
                    "running"
                }
            }
        };
        let started_at = existing_started_at
            .as_deref()
            .unwrap_or(event.started_at.as_str());
        let ended_at = matches!(
            event.event_type,
            TraceEventType::FinalResponse | TraceEventType::Error
        )
        .then(|| {
            event
                .ended_at
                .as_deref()
                .unwrap_or(event.started_at.as_str())
        });
        self.conn
            .execute(
                "INSERT INTO agent_runs (
                    run_id, session_id, status, started_at, ended_at, final_summary
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(run_id) DO UPDATE SET
                    session_id = excluded.session_id,
                    status = excluded.status,
                    ended_at = COALESCE(excluded.ended_at, agent_runs.ended_at),
                    final_summary = COALESCE(excluded.final_summary, agent_runs.final_summary)",
                params![
                    event.task_id.as_str(),
                    session_id,
                    status,
                    started_at,
                    ended_at,
                    event.output_summary.as_deref(),
                ],
            )
            .map_err(sql_error)?;
        Ok(())
    }
}

struct MessageRow {
    id: String,
    task_id: String,
    role: String,
    content: String,
    status: Option<String>,
    code_links_json: Option<String>,
    attachments_json: Option<String>,
    created_at: String,
}

struct TraceEventRow {
    id: String,
    task_id: String,
    step_index: u32,
    event_type_text: String,
    tool_name: Option<String>,
    title: String,
    input_json: Option<String>,
    output_json: Option<String>,
    output_summary: Option<String>,
    started_at: String,
    ended_at: Option<String>,
    duration_ms: Option<u64>,
    status_text: String,
}

impl TraceEventRow {
    fn into_event(self) -> Result<ToolTraceEvent, String> {
        Ok(ToolTraceEvent {
            id: self.id,
            task_id: self.task_id,
            step_index: self.step_index,
            event_type: enum_from_text(&self.event_type_text)?,
            tool_name: self.tool_name,
            title: self.title,
            input: decode_optional_json(self.input_json)?,
            output: decode_optional_json(self.output_json)?,
            output_summary: self.output_summary,
            started_at: self.started_at,
            ended_at: self.ended_at,
            duration_ms: self.duration_ms,
            status: enum_from_text(&self.status_text)?,
        })
    }
}

fn session_positions(
    task_ids_by_project_id: &HashMap<String, Vec<String>>,
) -> HashMap<String, i64> {
    let mut positions = HashMap::new();
    for task_ids in task_ids_by_project_id.values() {
        for (index, task_id) in task_ids.iter().enumerate() {
            positions.insert(task_id.clone(), index as i64);
        }
    }
    positions
}

fn default_messages_loaded() -> bool {
    true
}

fn enum_text<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_value(value)
        .map_err(json_error)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "enum serialization did not produce a string".to_string())
}

fn enum_from_text<T: DeserializeOwned>(text: &str) -> Result<T, String> {
    serde_json::from_value(Value::String(text.to_string())).map_err(json_error)
}

fn optional_json<T: Serialize>(value: &Option<T>) -> Result<Option<String>, String> {
    value
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(json_error)
}

fn decode_optional_json<T: DeserializeOwned>(value: Option<String>) -> Result<Option<T>, String> {
    value
        .map(|value| serde_json::from_str(&value))
        .transpose()
        .map_err(json_error)
}

fn sql_error(error: rusqlite::Error) -> String {
    format!("SQLite operation failed: {error}")
}

fn json_error(error: serde_json::Error) -> String {
    format!("JSON serialization failed: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_conversation_and_trace_in_separate_tables() {
        let path = std::env::temp_dir().join(format!(
            "codeforge-history-test-{}.sqlite3",
            uuid::Uuid::new_v4()
        ));
        let mut store = HistoryStore::load(path.clone()).unwrap();
        let trace = ToolTraceEvent {
            id: "trace-1".to_string(),
            task_id: "run-1".to_string(),
            step_index: 1,
            event_type: TraceEventType::ToolResult,
            tool_name: Some("workspace/read_file".to_string()),
            title: "tool_result".to_string(),
            input: Some(serde_json::json!({ "path": "src/main.rs" })),
            output: Some(serde_json::json!({ "file": "src/main.rs" })),
            output_summary: Some("file=src/main.rs".to_string()),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            ended_at: Some("2026-01-01T00:00:01Z".to_string()),
            duration_ms: Some(10),
            status: TraceStatus::Success,
        };
        let mut tasks_by_id = HashMap::new();
        tasks_by_id.insert(
            "session-1".to_string(),
            AgentTaskRecord {
                id: "session-1".to_string(),
                project_id: "project-1".to_string(),
                prompt: "read file".to_string(),
                messages: vec![
                    ChatMessageRecord {
                        id: "message-1".to_string(),
                        task_id: "session-1".to_string(),
                        role: "user".to_string(),
                        content: "read file".to_string(),
                        status: None,
                        code_links: None,
                        attachments: None,
                        trace_events: None,
                        created_at: "2026-01-01T00:00:00Z".to_string(),
                    },
                    ChatMessageRecord {
                        id: "message-2".to_string(),
                        task_id: "run-1".to_string(),
                        role: "assistant".to_string(),
                        content: "done".to_string(),
                        status: Some("completed".to_string()),
                        code_links: None,
                        attachments: None,
                        trace_events: Some(vec![trace.clone()]),
                        created_at: "2026-01-01T00:00:02Z".to_string(),
                    },
                ],
                trace_events: vec![trace.clone()],
                status: "completed".to_string(),
                messages_loaded: true,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:02Z".to_string(),
            },
        );
        let state = WorkspaceHistoryState {
            active_project_id: Some("project-1".to_string()),
            current_workspace_task_id: Some("session-1".to_string()),
            tasks_by_id,
            task_ids_by_project_id: HashMap::from([(
                "project-1".to_string(),
                vec!["session-1".to_string()],
            )]),
        };

        store.save_workspace_history(&state, true).unwrap();
        let loaded = store.load_workspace_history().unwrap();
        let loaded_task = loaded.tasks_by_id.get("session-1").unwrap();
        assert!(!loaded_task.messages_loaded);
        assert_eq!(loaded_task.messages.len(), 0);
        assert_eq!(loaded_task.trace_events.len(), 0);
        assert_eq!(loaded_task.created_at, "2026-01-01T00:00:00Z");
        assert_eq!(loaded_task.updated_at, "2026-01-01T00:00:02Z");

        let loaded_task = store.load_workspace_session("session-1").unwrap();
        assert!(loaded_task.messages_loaded);
        assert_eq!(loaded_task.messages.len(), 2);
        assert_eq!(loaded_task.trace_events.len(), 1);
        assert_eq!(
            loaded_task.messages[1].trace_events.as_ref().unwrap()[0].tool_name,
            Some("workspace/read_file".to_string())
        );
        assert_eq!(store.list_trace_events("run-1").unwrap().len(), 1);
        store
            .update_agent_run_metadata("run-1", Some("provider-1"), Some("model-1"))
            .unwrap();
        let metadata = store
            .conn
            .query_row(
                "SELECT provider_id, model_id FROM agent_runs WHERE run_id = ?1",
                params!["run-1"],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .unwrap();
        assert_eq!(metadata, ("provider-1".to_string(), "model-1".to_string()));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn load_marks_abandoned_running_runs_failed() {
        let path = std::env::temp_dir().join(format!(
            "codeforge-history-test-{}.sqlite3",
            uuid::Uuid::new_v4()
        ));
        {
            let mut store = HistoryStore::load(path.clone()).unwrap();
            let task = AgentTaskRecord {
                id: "session-1".to_string(),
                project_id: "project-1".to_string(),
                prompt: "ask".to_string(),
                messages: vec![ChatMessageRecord {
                    id: "message-1".to_string(),
                    task_id: "run-1".to_string(),
                    role: "assistant".to_string(),
                    content: "Thinking...".to_string(),
                    status: Some("running".to_string()),
                    code_links: None,
                    attachments: None,
                    trace_events: None,
                    created_at: "2026-01-01T00:00:00Z".to_string(),
                }],
                trace_events: Vec::new(),
                status: "running".to_string(),
                messages_loaded: true,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
            };
            store.save_workspace_session(&task, 0).unwrap();
            store
                .insert_trace_event(
                    "session-1",
                    &ToolTraceEvent {
                        id: "trace-1".to_string(),
                        task_id: "run-1".to_string(),
                        step_index: 1,
                        event_type: TraceEventType::LlmRequest,
                        tool_name: None,
                        title: "llm_request:1".to_string(),
                        input: None,
                        output: None,
                        output_summary: Some("model=test".to_string()),
                        started_at: "2026-01-01T00:00:00Z".to_string(),
                        ended_at: Some("2026-01-01T00:00:00Z".to_string()),
                        duration_ms: Some(0),
                        status: TraceStatus::Success,
                    },
                )
                .unwrap();
        }

        let store = HistoryStore::load(path.clone()).unwrap();
        let run = store
            .conn
            .query_row(
                "SELECT status, ended_at, final_summary FROM agent_runs WHERE run_id = ?1",
                params!["run-1"],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(run.0, "failed");
        assert!(run.1.is_some());
        assert!(run
            .2
            .as_deref()
            .is_some_and(|summary| summary.contains("abandoned before completion")));

        let traces = store.list_trace_events("run-1").unwrap();
        assert_eq!(traces.len(), 2);
        assert_eq!(traces[1].title, "agent_run_abandoned");
        assert!(matches!(traces[1].status, TraceStatus::Failed));

        let task = store.load_workspace_session("session-1").unwrap();
        assert_eq!(task.status, "failed");
        assert_eq!(task.messages[0].status.as_deref(), Some("failed"));

        let _ = std::fs::remove_file(path);
    }
}
