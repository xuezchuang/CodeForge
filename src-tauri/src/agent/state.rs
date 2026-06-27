use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunState {
    Start,
    SelectModel,
    PrepareContext,
    MaybeCompactContext,
    StartMcp,
    PrepareModelRequest,
    RequestModel,
    HandleModelResponse,
    ExecuteToolCalls,
    RequestFinalWithoutTools,
    Finalize,
    Completed,
    Failed,
}

impl AgentRunState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::SelectModel => "select_model",
            Self::PrepareContext => "prepare_context",
            Self::MaybeCompactContext => "maybe_compact_context",
            Self::StartMcp => "start_mcp",
            Self::PrepareModelRequest => "prepare_model_request",
            Self::RequestModel => "request_model",
            Self::HandleModelResponse => "handle_model_response",
            Self::ExecuteToolCalls => "execute_tool_calls",
            Self::RequestFinalWithoutTools => "request_final_without_tools",
            Self::Finalize => "finalize",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTransition {
    pub from: AgentRunState,
    pub to: AgentRunState,
    pub reason: String,
    pub round_index: Option<usize>,
}
