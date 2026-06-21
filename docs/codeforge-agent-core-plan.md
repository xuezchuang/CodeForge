# CodeForge Agent Core Plan

## Background

CodeForge Desktop and CodeForge TUI currently do not share one agent runtime.

Desktop runs through the Tauri backend:

```text
React UI
-> Tauri commands
-> src-tauri/src/commands.rs
-> src-tauri/src/agent_runner.rs
-> tool_registry / tool_trace / history_store
```

TUI runs through the Codex-style app-server/TUI stack:

```text
src-tui/src/main.rs
-> codeforge_tui::run_codeforge_main()
-> codex app-server protocol/client/event model
```

The two paths use similar product language, but they are not one runtime. The desktop runner owns its own provider calls, stream parsing, tool execution loop, and trace mapping. The TUI code owns a separate app-server protocol integration.

## Current Problems

`src-tauri/src/agent_runner.rs` has become a large mixed-responsibility file. It currently handles:

- agent run setup and round progression
- provider selection and request building
- OpenAI-compatible chat completion calls
- streaming response parsing
- tool call delta merging
- tool execution
- trace event creation
- final response extraction
- timeout and round-budget behavior
- error reporting and status mapping

This makes failures hard to isolate. A provider streaming bug can appear as an empty tool name, a failed tool result, an empty final response, or a warning run status. The file also makes it hard for Desktop and TUI to converge because the runtime logic is not expressed as a reusable core.

## Product Direction

CodeForge should own a small, explicit agent core instead of making Desktop and TUI maintain separate agent behavior.

The goal is not to import the full Codex core runtime. Codex remains a reference for event lifecycle, terminal interaction, and protocol design. CodeForge should keep ownership of:

- provider and credential selection
- local workspace and Visual Studio tools
- trace creation and persistence
- coding-focused safety policy
- CodeForge tool schemas and results
- desktop and TUI product behavior

## Target Architecture

Introduce a CodeForge-owned agent core with clear boundaries:

```text
src-tauri/src/agent/
  mod.rs
  runner.rs
  loop_state.rs
  messages.rs
  errors.rs
  budget.rs

src-tauri/src/agent/provider/
  mod.rs
  openai_chat.rs
  anthropic.rs
  ollama.rs
  codex_cli.rs
  request_builder.rs

src-tauri/src/agent/stream/
  mod.rs
  openai_sse.rs
  tool_call_merge.rs
  events.rs

src-tauri/src/agent/tools/
  mod.rs
  executor.rs
  result_summary.rs

src-tauri/src/agent/trace/
  mod.rs
  mapper.rs
```

The current `agent_runner.rs` should become a thin orchestration layer or be replaced by `agent/runner.rs` after migration.

## Core Concepts

### Agent Runner

Responsible only for orchestration:

- create the run
- build the initial message list
- advance model rounds
- execute requested tools
- decide whether to continue or finish
- return the final run summary

It should not parse SSE chunks, directly format trace JSON, or know provider-specific response quirks.

### Provider Layer

Responsible for provider-specific request/response behavior:

- OpenAI-compatible `/v1/chat/completions`
- Anthropic-style calls if kept
- Ollama calls
- CodeForge gateway behavior
- Codex CLI provider adapter

Provider output should be normalized into CodeForge events instead of leaking raw provider shapes into the runner.

### Stream Layer

Responsible for incremental model output:

- parse SSE frames
- merge chat completion deltas
- merge tool call fragments
- accumulate usage and finish reason
- emit typed stream events

The stream layer should have fixture tests from real gateway/provider chunks. This is the most important near-term hardening area because tool-call streaming bugs directly break agent execution.

### Tool Layer

Responsible for CodeForge tool invocation:

- validate tool names and arguments
- call `tool_registry`
- normalize success/failure result shapes
- produce concise summaries for trace display

The product should continue avoiding arbitrary shell execution. New automation should be implemented as explicit safe tools.

### Trace Layer

Responsible for mapping agent events to product trace:

- model request started/completed/failed
- model message deltas
- reasoning/thinking deltas
- tool call planned
- tool execution started/completed/failed
- budget/timeout/context warnings

Trace should consume normalized events, not provider-specific response internals.

## Event Model

Use a typed internal event model as the boundary between provider/stream/tool logic and UI/trace logic.

Example shape:

```text
AgentEvent
  RunStarted
  ModelRequestStarted
  ModelMessageDelta
  ModelReasoningDelta
  ModelToolCallDelta
  ModelRequestCompleted
  ToolCallStarted
  ToolCallCompleted
  ToolCallFailed
  BudgetWarning
  RunCompleted
  RunFailed
```

Desktop can map these events to Tauri trace events. TUI can later map the same events to app-server notifications. This is the bridge that lets Desktop and TUI share the core without forcing either UI to own provider details.

## Migration Plan

### Phase 1: Extract Streaming

Move OpenAI-compatible streaming code out of `agent_runner.rs` into:

```text
src-tauri/src/agent/stream/openai_sse.rs
src-tauri/src/agent/stream/tool_call_merge.rs
src-tauri/src/agent/stream/events.rs
```

Add tests for:

- content delta accumulation
- reasoning delta accumulation
- tool call chunks with explicit index
- tool call chunks without index
- split function name and arguments
- stream usage frames
- provider `data: {"error": ...}` frames

### Phase 2: Extract Provider Client

Move chat completion request/response logic into `agent/provider/openai_chat.rs`.

The runner should call a provider trait and receive normalized model events/results.

### Phase 3: Extract Tool Execution

Move tool invocation and summary formatting into `agent/tools`.

The runner should not construct detailed trace payloads for every tool result.

### Phase 4: Extract Trace Mapping

Move trace construction into `agent/trace/mapper.rs`.

The trace mapper should convert normalized `AgentEvent` values into `ToolTraceEvent` records.

### Phase 5: Thin Runner

Reduce the runner to state-machine orchestration:

```text
prepare messages
loop:
  request model
  collect events
  execute tool calls
  append tool results
  finish or continue
```

At this point the old `agent_runner.rs` can either become `agent/runner.rs` or remain as a compatibility wrapper.

### Phase 6: TUI Convergence

After the desktop core is stable, make TUI consume the CodeForge agent core through an adapter:

```text
CodeForge TUI
-> CodeForge app-server adapter
-> CodeForge agent core
-> CodeForge provider/tool/trace systems
```

This should be incremental. Do not replace the TUI all at once.

## Verification Strategy

Each migration phase should keep behavior stable and include focused tests.

Required checks for code changes:

```text
build-codeforge-cli.bat
build-release.bat
```

Additional focused checks:

- Rust unit tests for stream parsing and tool call merging
- fixture tests using captured gateway stream chunks
- UI checks for trace raw/request/response/tool displays
- regression test for empty final response when the model only emits tool calls
- regression test for tool calls with missing stream index

## Non-Goals

- Do not import the whole Codex core runtime.
- Do not make CodeForge a thin wrapper around Codex.
- Do not expose arbitrary shell execution as a generic tool.
- Do not collapse trace detail for simpler UI rendering.
- Do not rewrite Desktop and TUI in one large change.

## Decision

Proceed with a staged refactor. Start with streaming and provider extraction because those are the current failure points and the best boundary for reducing risk.

The immediate next implementation step should be:

```text
Extract stream parsing and tool-call merge logic from agent_runner.rs into agent/stream with fixture tests.
```
