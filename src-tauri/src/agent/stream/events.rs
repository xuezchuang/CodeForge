/// Normalized stream events emitted by the OpenAI-compatible SSE parser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamEvent {
    ContentDelta(String),
    ReasoningDelta(String),
    ToolCallDelta,
    Usage,
    Finished(String),
    Error(String),
}
