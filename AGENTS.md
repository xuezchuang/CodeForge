# AGENTS.md

Project-local guidance for coding agents working in this repository.

## Product Goal

CodeForge is a Windows Tauri desktop app in the same broad category as Codex Desktop and Claude Desktop, but with a narrower product scope:

- Focus on coding workflows only.
- Make the agent process fully transparent through trace UI.
- Show how model calls, tool calls, skills, MCP-style integrations, and local IDE actions are invoked.
- Prefer an inspectable engineering tool over a general chat assistant.

The current development focus is tool-calling experiments. Treat trace quality as product behavior, not as debug-only output.

## Agent Safety Policy

The project agent is a code editing assistant only. It may search files and code, read workspace files, analyze code, modify code, apply patches, show diffs, and read IDE/compiler/linter diagnostics when available.

Do not automatically execute scripts, shell commands, installers, package managers, build commands, test commands, deploy commands, or unsafe tools. Do not install packages, download and execute scripts, or access files outside the workspace.

If build or test support is needed later, implement explicit safe tools such as `build_solution` or `run_tests` with fixed command templates, workspace confinement, trace output, and user confirmation. Do not expose arbitrary shell execution.

## Architecture Boundaries

- React owns UI rendering and calls typed Tauri commands from `src/api/tauriApi.ts`.
- Rust owns local state, settings, providers, agent runs, tool execution, trace creation, Visual Studio launch, VS bridge registration, and code-link resolution.
- Keep tool definitions and tool execution server-side unless a feature is clearly UI-only.
- Do not put secret values into traces. Mask API keys and other credentials.
- Browser-only Vite mode (`npm run dev`) is for frontend layout work only. Tauri commands require `npm run tauri dev` or a built desktop app.

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

## CLI Coding Reference

For CLI implementation work in this repository, use `D:\code\CodeForge` as the local reference checkout for Codex CLI style and structure, especially:

```text
D:\code\CodeForge\AGENTS.md
D:\code\CodeForge\codex-rs\cli\
D:\code\CodeForge\codex-rs\cli\src\main.rs
D:\code\CodeForge\codex-rs\cli\tests\
```

Reference that code for command organization, argument parsing shape, human-vs-JSON output separation, exit/error behavior, and focused CLI tests. Do not add a direct dependency on the reference checkout, vendor copied files, or change `D:\code\CodeForge` unless the user explicitly asks.

Current local CLI entry points are:

```text
src-tauri\src\bin\codeforge.rs
src-tauri\src\cli.rs
src-tauri\src\agent_runner.rs
src-tauri\src\codex_cli_runner.rs
src-tauri\src\tool_registry.rs
build-codeforge-cli.bat
```

Keep CLI changes aligned with this repository's Rust/Tauri ownership model. The CLI may share desktop state and agent orchestration, but it must not bypass trace creation, provider selection rules, credential masking, workspace boundaries, or the agent safety policy above. If a CLI feature needs build, test, shell, or IDE automation, implement it as an explicit safe tool with fixed command templates, workspace confinement, trace output, and user confirmation instead of exposing generic shell execution.

## CodeForge CLI Direction

CodeForge CLI is a first-class product surface, not just a helper launcher.

The first CLI milestone is to make the CodeForge command-line experience closely match the Codex CLI/TUI interaction model while still running CodeForge's own Rust/Tauri backend, provider settings, workspace state, trace system, Visual Studio bridge, and CodeForge-owned tools.

Use the in-repository Codex checkout as the primary reference for CLI/TUI behavior:

```text
codex\codex-rs\cli\
codex\codex-rs\tui\
codex\codex-rs\app-server-protocol\
```

The goal is not to embed the whole Codex core runtime as CodeForge's architecture. CodeForge should selectively copy, adapt, or reimplement the parts it needs.

Prioritize the CLI surface first:

- Codex-like interactive terminal UI.
- Composer/input behavior.
- Slash command popup and command routing.
- `/goal` style goal management.
- Status/footer behavior where useful.
- Human-readable output vs JSON output separation.
- Clear command parsing and error behavior.
- Traceable agent/tool activity.

Then build the tool layer interface:

- Tool schema definitions.
- Tool invocation request/response types.
- Tool result and error shape.
- Approval request shape.
- Trace event mapping for tool calls.
- A small registry for CodeForge-owned tools.

Only implement tools CodeForge actually needs. Do not bring over broad Codex features just because they exist.

CodeForge tools should support coding workflows, especially local C++ / Visual Studio project understanding. Preferred early tools include:

```text
workspace/search
workspace/read_file
workspace/apply_patch
vs/current_solution
vs/current_document
vs/current_selection
vs/list_projects
vs/find_definition
vs/find_references
vs/get_error_list
goal/get
goal/set
goal/clear
```

Do not expose arbitrary shell execution as a generic tool. If build or test support is needed, implement explicit safe tools with fixed command templates, workspace confinement, user confirmation, and trace output.

Copying or adapting Codex CLI/TUI/tool-interface code is allowed when it directly serves the CodeForge CLI goal.

Rules:

- Keep copied changes scoped.
- Preserve license/header attribution when present.
- Prefer copying small coherent modules over dragging in large dependency chains.
- Rename/adapt types to CodeForge ownership where appropriate.
- Do not make CodeForge a thin wrapper around `cf`, `codex`, or `cargo run --bin codex`.
- Do not pull in Codex core/runtime dependencies unless explicitly requested for a specific reason.

CodeForge owns:

```text
CLI command routing
terminal rendering integration
project/workspace state
provider and credential selection
trace creation and storage
Visual Studio bridge/tool integration
CodeForge tool registry
goal state used by CodeForge
```

Codex is a reference for:

```text
CLI/TUI interaction design
slash command behavior
goal command behavior
tool call interface shape
approval flow ideas
trace/event presentation ideas
```

## Product Direction

CodeForge is a local C++ / Visual Studio coding agent with VSIX semantic integration, workspace cache, build-error repair loop, and traceable tool execution.

Do not treat this as a generic chat wrapper. The product advantage should be C++ / Visual Studio project understanding:

- Current solution, project, active document, and selection.
- Symbol definitions and references.
- Caller/callee and override information.
- Project-to-file ownership.
- Active build configuration.
- Compiler error context.
- Clickable code links back into Visual Studio.
- Tool execution trace and token usage.

## Trace Rules

Trace is the main product surface.

- Every meaningful agent step should be represented as a trace event.
- Tool calls must show tool name, input arguments, result or error, status, and duration when available.
- LLM calls must show request/response shape, model/provider, token usage, and cache usage when reported by the provider.
- If adding skills, MCP servers, or external adapters, model them as traceable steps instead of hidden side effects.
- Keep raw payload access available when possible, but summarize important fields for quick reading.
- Prefer adding explicit trace data over inferring from rendered text.
- Failed tool/model steps should be visible as failed trace events, not hidden behind a generic chat error.

## Tool And Skill Experiments

- The current tool-call test path is intentional. Preserve it as a small, inspectable workflow while tool calling is being evaluated.
- Keep demo tools small and deterministic unless the user asks for real external integration.
- When adding a new tool, define its schema, execution, trace event shape, and UI summary together.
- When testing MCP/skill-like behavior, make the adapter boundary explicit: what input is sent, what output comes back, what failed, and how long it took.
- Do not add broad automation or multi-domain assistant features unless they directly support coding workflow traceability.

## Planned Semantic VSIX Tools

The current bridge opens files. The next stage is to expose semantic C++ project tools from Visual Studio:

```text
vs.current_solution
vs.current_document
vs.current_selection
vs.list_projects
vs.list_project_files
vs.find_definition
vs.find_references
vs.get_error_list
```

Later tools may include:

```text
vs.find_callers
vs.find_callees
vs.find_overrides
vs.find_derived_classes
vs.get_build_configuration
vs.prepare_context
```

The VSIX should remain a semantic bridge. Model orchestration, task planning, patching, trace storage, token accounting, and provider configuration belong in the Desktop / Agent Host side.

## Coding Rules

- Keep changes surgical. Do not refactor adjacent UI or Rust modules unless the requested change needs it.
- Prefer existing patterns over new abstractions.
- Maintain typed data flow across Rust structs, Tauri command outputs, TypeScript types, and React view models.
- If changing trace event payloads, update all affected layers in the same change.
- Keep UI dense and utilitarian. This app is a workbench, not a landing page.
- Do not silently remove existing trace detail to simplify a new view.
- Prefer patch-based edits over full-file rewrites.
- Keep model orchestration outside the VSIX.
- Treat Visual Studio / clangd / project files as the source of truth.

## Verification

After any code change in this repository, run both compile checks before reporting completion:

```text
build-codeforge-cli.bat
build-release.bat
```

Run `build-codeforge-cli.bat` first for the CLI binary, then `build-release.bat` for the desktop Release binary. `build-release-installer.bat` is installer-only and should not be executed unless installer output is explicitly required.

These are the required verification checks for code edits. If either build fails, inspect the concrete error, fix the cause when it is in scope, and rerun the failed build. If either build cannot be run, report the blocker explicitly.

For documentation-only edits, use the smallest safe check that proves the change, such as direct file review or `git diff --check`.

## Reporting

When reporting code locations to the user, use direct clickable VS Code links, one per line. Do not put them in a fenced code block, and do not use file cards or rich previews as the primary format:

[TraceDrawer.tsx:130](vscode://file/D:/code/snowAgents/src/components/TraceDrawer.tsx:130)
[agent_runner.rs:249](vscode://file/D:/code/snowAgents/src-tauri/src/agent_runner.rs:249)

Summarize:

- Changed: file -> behavior changed.
- Validation: command -> result.
- Notes: remaining risk or what was not exercised.
