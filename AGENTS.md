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

Do not automatically execute scripts, shell commands, installers, package managers, build commands, test commands, deploy commands, or unsafe tools. Do not run `.bat`, `.cmd`, `.ps1`, `.sh`, `.exe`, `.msi`, or similar files. Do not use generic shell tools such as `execute_command`, `run_shell`, `terminal`, or `powershell`. Do not install packages, download and execute scripts, or access files outside the workspace.

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

After any code change in this repository, compile the main desktop release before reporting completion:

```text
npm run tauri build
```

The default test flow is to run `build-release.bat` for Release compile verification first. `build-release-installer.bat` is installer-only and should not be executed unless installer output is explicitly required.

This is the required release verification for code edits. If the release build fails, inspect the concrete error, fix the cause when it is in scope, and rerun the release build. If the build cannot be run, report the blocker explicitly.

For documentation-only edits, use the smallest safe check that proves the change, such as direct file review or `git diff --check`.

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
