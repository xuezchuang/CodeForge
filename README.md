# CodeForge

CodeForge is a Windows-first desktop workspace for running local coding agents
against real projects. It combines a React/Tauri desktop app, a Rust agent
runtime, a Codex-derived CLI/TUI, shared workspace tooling, and an optional
Visual Studio VSIX bridge.

The repository directory may still be named `snowAgents`, but the shipped
desktop product is `CodeForge`.

## Features

- Project registry for local repositories, Visual Studio solutions, Unreal
  project files, and project-specific build commands.
- Workspace chat UI with persistent conversation history, trace events, and
  code-link navigation.
- Agent runtime with provider selection, tool-call tracing, subagents, goal
  state, MCP server tools, workspace tools, git tools, document/PPTX readers,
  and optional shell execution.
- Provider configuration for Codex CLI, OpenAI-compatible endpoints, CodeBuddy,
  Claude, DeepSeek, MiniMax, Ollama, and local gateway profiles.
- Visual Studio integration through a loopback VSIX bridge for solution context,
  file opening, project/file listing, and search endpoints.
- `cf.cmd` launcher for the bundled Codex-derived terminal experience, using
  `%USERPROFILE%\.codeforge` as its config home.

## Repository Layout

```text
src/                         React + Vite desktop UI
src-tauri/                   Tauri 2 backend and desktop agent runtime
src-tui/                     CodeForge CLI/TUI binary
crates/codeforge-core/       Shared workspace, file, goal, and office tooling
vsix/SnowAgent.VSBridge/     Visual Studio bridge extension
codex/                       Codex-derived git submodule used by the CLI/TUI
docs/                        Architecture and bridge protocol notes
tools/                       Local helper and smoke-test scripts
```

## Requirements

- Windows
- Node.js 22 or newer
- Rust stable toolchain with Cargo
- Tauri 2 prerequisites for Windows desktop builds
- Visual Studio 2026 Community or compatible MSBuild toolchain for the VSIX
  bridge
- .NET/MSBuild restore support for `vsix\SnowAgent.VSBridge`

## Bootstrap

From the repository root:

```powershell
git submodule update --init --recursive
npm install
cargo fetch --manifest-path src-tauri\Cargo.toml
cargo fetch --manifest-path src-tui\Cargo.toml
dotnet restore vsix\SnowAgent.VSBridge\SnowAgent.VSBridge.csproj
```

The `codex/` submodule is required by the CLI/TUI. If `cf.cmd` reports that the
submodule workspace is missing, run the submodule command again.

## Run the Desktop App

Use the Tauri dev command when working on the actual app:

```powershell
npm run tauri dev
```

`npm run dev` starts only the Vite frontend. It is useful for pure UI work, but
Tauri commands and backend-backed screens will not work without `npm run tauri
dev`.

## Build

Build the release desktop executable without installer bundles:

```powershell
.\build-release.bat
```

Output:

```text
src-tauri\target\release\codeforge-desktop.exe
```

Build installer bundles:

```powershell
.\build-release-installer.bat
```

Outputs:

```text
src-tauri\target\release\bundle\nsis\CodeForge_0.1.0_x64-setup.exe
src-tauri\target\release\bundle\msi\CodeForge_0.1.0_x64_en-US.msi
```

Build the CodeForge CLI binary:

```powershell
.\build-codeforge-cli.bat
```

Output:

```text
src-tauri\target\release\codeforge.exe
```

## Common Checks

```powershell
npm run build
npm run lint
cargo test --manifest-path src-tauri\Cargo.toml
cargo test --manifest-path src-tui\Cargo.toml
```

For repository changes that affect the desktop product, the required final
verification command is:

```powershell
D:\code\snowAgents\build-release.bat
```

## Configuration and Data

CodeForge stores runtime state locally:

```text
%LOCALAPPDATA%\SnowAgentDesktop\projects.json
%LOCALAPPDATA%\SnowAgentDesktop\codeforge.sqlite3
```

Provider and MCP configuration live under:

```text
%USERPROFILE%\.codeforge\config.toml
%USERPROFILE%\.codeforge\settings.json
```

`settings.example.json` shows the legacy/provider JSON shape. CodeBuddy model
entries can also be imported from:

```text
%USERPROFILE%\.codebuddy\models.json
```

MCP servers are configured in `config.toml` with `[mcp_servers.<id>]` sections.
The runtime supports stdio and HTTP MCP transports, manual connect/disconnect,
tool listing, and exposing connected MCP tools to agent runs.

## Visual Studio Bridge

The desktop backend starts a local registration server at:

```text
http://127.0.0.1:39000
```

The VSIX starts its own dynamic loopback endpoint, registers the active Visual
Studio process with the desktop app, and exposes `POST /openFile` plus related
workspace/search endpoints. See `docs\vs-bridge-protocol.md` for the protocol
details.

Build the VSIX with MSBuild:

```powershell
& "C:\Program Files\Microsoft Visual Studio\18\Community\MSBuild\Current\Bin\MSBuild.exe" vsix\SnowAgent.VSBridge\SnowAgent.VSBridge.csproj /restore /p:Configuration=Debug
```

Debugging flow:

1. Start CodeForge Desktop with `npm run tauri dev`.
2. Open `vsix\SnowAgent.VSBridge\SnowAgent.VSBridge.sln` in Visual Studio.
3. Start the VSIX debug profile, which launches the experimental Visual Studio
   instance.
4. Open a solution that matches a CodeForge project.
5. The project should move to the bridge-connected state in the desktop app.

## CLI Launcher

Run the bundled terminal agent from any workspace:

```powershell
.\cf.cmd
```

The launcher forwards the caller's current directory to the bundled Codex
binary and keeps its config isolated in `%USERPROFILE%\.codeforge`.

## License

MIT. See `LICENSE`.
