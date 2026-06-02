# AGENTS.md

Project-local guidance for coding agents working in this repository.

## Product Goal

SnowAgent Desktop is a Windows Tauri desktop app in the same broad category as
Codex Desktop and Claude Desktop, but with a narrower product scope:

- Focus on coding workflows only.
- Make the agent process fully transparent through trace UI.
- Show how model calls, tool calls, skills, MCP-style integrations, and local
  IDE actions are invoked.
- Prefer an inspectable engineering tool over a general chat assistant.

The current development focus is tool-calling experiments. Treat trace quality
as product behavior, not as debug-only output.

## Architecture Boundaries

- React owns UI rendering and calls typed Tauri commands from `src/api/tauriApi.ts`.
- Rust owns local state, settings, providers, agent runs, tool execution, trace
  creation, Visual Studio launch, VS bridge registration, and code-link
  resolution.
- Keep tool definitions and tool execution server-side unless a feature is
  clearly UI-only.
- Do not put secret values into traces. Mask API keys and other credentials.
- Browser-only Vite mode (`npm run dev`) is for frontend layout work only.
  Tauri commands require `npm run tauri dev` or a built desktop app.

Useful entry points:

```text
src-tauri\src\agent_runner.rs
src-tauri\src\tool_registry.rs
src-tauri\src\tool_trace.rs
src-tauri\src\commands.rs
src\components\TraceDrawer.tsx
src\components\TraceEventRow.tsx
src\components\traceViewModel.ts
src\types\trace.ts
src\api\tauriApi.ts
```

## Trace Rules

Trace is the main product surface.

- Every meaningful agent step should be represented as a trace event.
- Tool calls must show tool name, input arguments, result or error, status, and
  duration when available.
- LLM calls must show request/response shape, model/provider, token usage, and
  cache usage when reported by the provider.
- If adding skills, MCP servers, or external adapters, model them as traceable
  steps instead of hidden side effects.
- Keep raw payload access available when possible, but summarize important
  fields for quick reading.
- Prefer adding explicit trace data over inferring from rendered text.
- Failed tool/model steps should be visible as failed trace events, not hidden
  behind a generic chat error.

## Tool And Skill Experiments

- The current tool-call test path is intentional. Preserve it as a small,
  inspectable workflow while tool calling is being evaluated.
- Keep demo tools small and deterministic unless the user asks for real external
  integration.
- When adding a new tool, define its schema, execution, trace event shape, and
  UI summary together.
- When testing MCP/skill-like behavior, make the adapter boundary explicit:
  what input is sent, what output comes back, what failed, and how long it took.
- Do not add broad automation or multi-domain assistant features unless they
  directly support coding workflow traceability.

## Coding Rules

- Keep changes surgical. Do not refactor adjacent UI or Rust modules unless the
  requested change needs it.
- Prefer existing patterns over new abstractions.
- Maintain typed data flow across Rust structs, Tauri command outputs,
  TypeScript types, and React view models.
- If changing trace event payloads, update all affected layers in the same
  change.
- Keep UI dense and utilitarian. This app is a workbench, not a landing page.
- Do not silently remove existing trace detail to simplify a new view.

## Verification

Use the smallest check that proves the change:

```powershell
npm run build
npm run lint
cargo check --manifest-path src-tauri\Cargo.toml
cargo test --manifest-path src-tauri\Cargo.toml
npm run tauri build
```

Notes:

- Run `npm run tauri build` as the default final verification after every
  implementation, UI, or behavior change in this repository. Do not stop at
  `npm run build` unless the user explicitly asks to skip release build or the
  release build is blocked; if blocked, report the command and blocker.
- Use `npm run tauri build` when the user asks for a release compile.
- Use `npm run tauri dev` for real desktop behavior and Tauri backend testing.
- Use `npm run dev` only for frontend-only layout checks; report that backend
  behavior was not exercised.

## Reporting

When reporting code locations to the user, use copyable Visual Studio style:

```text
src\components\TraceDrawer.tsx:130
src-tauri\src\agent_runner.rs:249
```

Summarize:

- Changed: file -> behavior changed.
- Validation: command -> result.
- Notes: remaining risk or what was not exercised.
