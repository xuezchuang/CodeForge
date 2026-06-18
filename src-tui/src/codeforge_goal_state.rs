//! CodeForge goal state model for the standalone TUI.
//!
//! Mirrors `src-tauri/src/goal_state.rs` so the TUI carries the same
//! `GoalState` shape (objective, status, token budget, tokens used,
//! elapsed time) and serializes to the same `.codeforge/goal.json`
//! file. The TUI does not depend on the Tauri desktop crate, so the
//! TUI ships its own copy of the model. Phase 4 of the CodeForge TUI
//! backend-and-tooling plan reads this module on every `goal/get` call
//! and writes it on every `goal/set` and `goal/clear`.
//!
//! The persistence path is `<codeforge_home>/goal.json`, where
//! `codeforge_home` is resolved by the caller (typically
//! `~/.codeforge` or `$CODEFORGE_HOME`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Status of the active goal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GoalStatus {
    /// Goal is active and the agent should keep working toward it.
    Active,
    /// Goal is paused; the agent should not act on it until resumed.
    Paused,
    /// Goal is blocked; the agent cannot make progress without user input.
    Blocked,
    /// Goal is complete; no further work is needed.
    Complete,
}

impl GoalStatus {
    pub fn label(&self) -> &'static str {
        match self {
            GoalStatus::Active => "active",
            GoalStatus::Paused => "paused",
            GoalStatus::Blocked => "blocked",
            GoalStatus::Complete => "complete",
        }
    }
}

/// The active goal for a TUI session.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalState {
    /// The goal objective text.
    pub objective: String,
    /// Current status of the goal.
    pub status: GoalStatus,
    /// Optional token budget for the goal.
    pub token_budget: Option<i64>,
    /// Tokens consumed so far toward the goal.
    pub tokens_used: i64,
    /// Wall-clock seconds spent working on this goal.
    pub time_used_seconds: i64,
    /// RFC 3339 timestamp when the goal was created.
    pub created_at: String,
    /// RFC 3339 timestamp when the goal was last updated.
    pub updated_at: String,
}

impl GoalState {
    /// Create a new active goal with the given objective.
    pub fn new(objective: String) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            objective,
            status: GoalStatus::Active,
            token_budget: None,
            tokens_used: 0,
            time_used_seconds: 0,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    /// Mark the goal as paused.
    pub fn pause(&mut self) {
        self.status = GoalStatus::Paused;
        self.updated_at = Utc::now().to_rfc3339();
    }

    /// Resume a paused goal.
    pub fn resume(&mut self) {
        self.status = GoalStatus::Active;
        self.updated_at = Utc::now().to_rfc3339();
    }

    /// Mark the goal as complete.
    pub fn complete(&mut self) {
        self.status = GoalStatus::Complete;
        self.updated_at = Utc::now().to_rfc3339();
    }

    /// Mark the goal as blocked.
    pub fn block(&mut self) {
        self.status = GoalStatus::Blocked;
        self.updated_at = Utc::now().to_rfc3339();
    }

    /// Format elapsed seconds as a compact human-readable string.
    pub fn format_elapsed(seconds: i64) -> String {
        let seconds = seconds.max(0) as u64;
        if seconds < 60 {
            return format!("{seconds}s");
        }
        let minutes = seconds / 60;
        if minutes < 60 {
            return format!("{minutes}m");
        }
        let hours = minutes / 60;
        let remaining_minutes = minutes % 60;
        if hours >= 24 {
            let days = hours / 24;
            let remaining_hours = hours % 24;
            return format!("{days}d {remaining_hours}h {remaining_minutes}m");
        }
        if remaining_minutes == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h {remaining_minutes}m")
        }
    }
}

/// Compute the on-disk path for the goal file under `codeforge_home`.
pub fn goal_file(codeforge_home: &Path) -> PathBuf {
    codeforge_home.join("goal.json")
}

/// Load the goal state from `codeforge_home/goal.json`. Returns
/// `Ok(None)` when the file is absent or the JSON is malformed in a
/// recoverable way. Returns `Err` for I/O failures other than
/// "file not found" so callers can surface a real error.
pub fn load(codeforge_home: &Path) -> io::Result<Option<GoalState>> {
    let path = goal_file(codeforge_home);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(io::Error::new(
                err.kind(),
                format!("failed to read goal state {}: {err}", path.display()),
            ));
        }
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    match serde_json::from_str::<GoalState>(&text) {
        Ok(goal) => Ok(Some(goal)),
        Err(err) => {
            // A malformed goal file is treated as no goal so the TUI can
            // keep working. The caller may decide to surface a warning.
            tracing::warn!(
                error = %err,
                path = %path.display(),
                "ignoring malformed goal state"
            );
            Ok(None)
        }
    }
}

/// Persist the goal state to `codeforge_home/goal.json`. Creates the
/// directory if needed. Returns the canonical path on success.
pub fn save(codeforge_home: &Path, goal: &GoalState) -> io::Result<PathBuf> {
    fs::create_dir_all(codeforge_home)?;
    let path = goal_file(codeforge_home);
    let text = serde_json::to_string_pretty(goal).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to serialize goal state: {err}"),
        )
    })?;
    fs::write(&path, text)?;
    Ok(path)
}

/// Remove the goal file if present. Missing file is not an error so
/// clearing an already-cleared goal succeeds.
pub fn clear(codeforge_home: &Path) -> io::Result<bool> {
    let path = goal_file(codeforge_home);
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn new_goal_is_active() {
        let goal = GoalState::new("fix the bug".to_string());
        assert_eq!(goal.status, GoalStatus::Active);
        assert_eq!(goal.objective, "fix the bug");
        assert_eq!(goal.tokens_used, 0);
    }

    #[test]
    fn pause_and_resume_cycle() {
        let mut goal = GoalState::new("task".to_string());
        goal.pause();
        assert_eq!(goal.status, GoalStatus::Paused);
        goal.resume();
        assert_eq!(goal.status, GoalStatus::Active);
    }

    #[test]
    fn format_elapsed_seconds() {
        assert_eq!(GoalState::format_elapsed(0), "0s");
        assert_eq!(GoalState::format_elapsed(59), "59s");
        assert_eq!(GoalState::format_elapsed(60), "1m");
        assert_eq!(GoalState::format_elapsed(90 * 60), "1h 30m");
        assert_eq!(GoalState::format_elapsed(2 * 3600), "2h");
        assert_eq!(GoalState::format_elapsed(24 * 3600), "1d 0h 0m");
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let mut goal = GoalState::new("ship the milestone".to_string());
        goal.token_budget = Some(50_000);
        goal.tokens_used = 12_345;
        save(dir.path(), &goal).unwrap();
        let loaded = load(dir.path()).unwrap().expect("goal must load");
        assert_eq!(loaded.objective, "ship the milestone");
        assert_eq!(loaded.token_budget, Some(50_000));
        assert_eq!(loaded.tokens_used, 12_345);
        assert_eq!(loaded.status, GoalStatus::Active);
    }

    #[test]
    fn load_returns_none_when_file_absent() {
        let dir = TempDir::new().unwrap();
        assert!(load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn clear_removes_file_and_returns_true() {
        let dir = TempDir::new().unwrap();
        let goal = GoalState::new("temp".to_string());
        save(dir.path(), &goal).unwrap();
        assert!(goal_file(dir.path()).exists());
        assert!(clear(dir.path()).unwrap());
        assert!(!goal_file(dir.path()).exists());
        // Clearing again is a no-op, not an error.
        assert!(!clear(dir.path()).unwrap());
    }

    #[test]
    fn load_treats_malformed_json_as_no_goal() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path()).unwrap();
        fs::write(goal_file(dir.path()), "{ not json").unwrap();
        assert!(load(dir.path()).unwrap().is_none());
    }
}
