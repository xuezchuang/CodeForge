use super::state::{AgentRunState, AgentTransition};

#[derive(Debug)]
pub struct AgentRunSession {
    pub task_id: String,
    pub step_index: u32,
    state: AgentRunState,
    transitions: Vec<AgentTransition>,
}

impl AgentRunSession {
    pub fn new(task_id: impl Into<String>, state: AgentRunState, step_index: u32) -> Self {
        Self {
            task_id: task_id.into(),
            step_index,
            state,
            transitions: Vec::new(),
        }
    }

    pub fn state(&self) -> AgentRunState {
        self.state
    }

    pub fn transition(
        &mut self,
        to: AgentRunState,
        reason: impl Into<String>,
        round_index: Option<usize>,
    ) -> AgentTransition {
        let transition = AgentTransition {
            from: self.state,
            to,
            reason: reason.into(),
            round_index,
        };
        self.state = to;
        self.transitions.push(transition.clone());
        transition
    }

    pub fn transitions(&self) -> &[AgentTransition] {
        &self.transitions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_records_ordered_state_transitions() {
        let mut session = AgentRunSession::new("task", AgentRunState::Start, 7);

        session.transition(AgentRunState::PrepareModelRequest, "prepared", Some(0));
        session.transition(AgentRunState::RequestModel, "request ready", Some(0));

        assert_eq!(session.state(), AgentRunState::RequestModel);
        assert_eq!(session.step_index, 7);
        assert_eq!(session.transitions().len(), 2);
        assert_eq!(session.transitions()[0].from, AgentRunState::Start);
        assert_eq!(session.transitions()[1].to, AgentRunState::RequestModel);
    }
}
