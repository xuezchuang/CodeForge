//! CodeForge goal state model.
//!
//! Goal state is local application state stored per chat session, not in the
//! Codex runtime. The /goal slash command manipulates this state, and it is
//! later exposed to the agent through tools like goal/get, goal/set,
//! goal/clear.

use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Status of the active goal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

/// The active goal for a session.
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
}
