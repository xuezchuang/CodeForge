# CodeForge CLI/TUI Rebuild Plan

## Decision

Do not continue polishing the current "Codex desktop integration" direction.

The next result must be visible and simple:

```text
codeforge
  -> opens a real full-screen terminal TUI window
  -> looks and behaves close to Codex/cf/xcode style
  -> accepts input without layout glitches
  -> shows transcript, composer, slash popup, status/footer
  -> exits cleanly and restores the terminal
```

Tool exchange, full protocol parity, rich trace mapping, and desktop/CLI
unification can come after that. The first milestone is not a complete agent
runtime. The first milestone is that the CLI starts and the TUI feels normal.

## Review Of The Current Plan

The existing plan has the right broad direction, but the priority is still too
diffuse:

- It talks too much about tool interface shape before the TUI is acceptable.
- It spreads work across parser, goals, tools, traces, and old demo cleanup.
- It treats "copy/adapt Codex TUI" as one phase instead of the main task.
- It does not make "a working normal window" the first acceptance gate.
- It risks preserving the current rough TUI just because it compiles.

The rewrite is stricter: first copy the Codex TUI shell closely enough that
CodeForge launches into a usable terminal app. Only then wire deeper behavior.

## Current Repo Facts

CodeForge already has CLI/TUI scaffolding:

```text
src-tauri\src\bin\codeforge.rs
src-tauri\src\cli.rs
src-tauri\src\tui\app.rs
src-tauri\src\tui\terminal.rs
src-tauri\src\tui\widgets\composer.rs
src-tauri\src\tui\widgets\footer.rs
src-tauri\src\tui\widgets\header.rs
src-tauri\src\tui\widgets\slash_popup.rs
src-tauri\src\tui\widgets\transcript.rs
src-tauri\src\slash_command.rs
src-tauri\src\goal_state.rs
src-tauri\src\tool_interface.rs
```

The reference implementation is in the in-repository Codex checkout:

```text
codex\codex-rs\cli\
codex\codex-rs\tui\
codex\codex-rs\app-server-protocol\
```

The current CodeForge TUI should be treated as a draft. Keep any parts that
help, but do not protect its layout or state model if copying Codex behavior is
faster and cleaner.

## Hard Product Target

The CLI must become a first-class CodeForge surface, not a wrapper.

Allowed:

```text
copy small coherent Codex TUI modules
adapt Codex widget behavior into CodeForge-owned modules
reuse ratatui/crossterm patterns
rename types to CodeForge ownership
stub backend events while the window is being proven
```

Not allowed:

```text
spawning cf.cmd as the normal implementation
spawning cargo run --bin codex as the normal implementation
dragging in Codex core/runtime just to show a screen
adding generic shell execution
hiding bad TUI behavior behind future protocol work
```

## First Acceptance Gate

Before tool work continues, this must pass manually:

```text
build-codeforge-cli.bat
target\release\codeforge.exe
```

Expected behavior:

```text
full-screen alternate-screen TUI opens
header is stable
transcript area is stable
composer is at the bottom and does not overlap text
typing works
Enter submits
Shift+Enter or equivalent multiline behavior is defined
/ opens a slash-command popup
arrow keys move selection/history without corrupting layout
/help shows command help inside the transcript
/status can show a local stub/status summary
/quit exits
Ctrl-C exits
terminal returns to normal after exit
```

If this gate fails, do not spend time on tool schema, approval shape, or trace
storage.

## Copy Strategy

Copy behavior in this order.

### 1. Terminal Lifecycle

Reference:

```text
codex\codex-rs\tui\src\tui\
codex\codex-rs\tui\src\custom_terminal.rs
```

CodeForge target:

```text
src-tauri\src\tui\terminal.rs
src-tauri\src\tui\app.rs
```

Goal:

```text
raw mode
alternate screen
panic/drop cleanup
resize handling
steady redraw loop
Windows terminal compatibility
```

This is the base. A broken terminal lifecycle makes every other feature look
bad.

### 2. App Frame And Layout

Reference:

```text
codex\codex-rs\tui\src\app.rs
codex\codex-rs\tui\src\chatwidget.rs
codex\codex-rs\tui\src\bottom_pane\
```

CodeForge target:

```text
src-tauri\src\tui\app.rs
src-tauri\src\tui\widgets\
```

Goal:

```text
stable vertical layout
scrollable transcript
bottom composer
slash popup above composer
footer/status line
no visual overlap on narrow terminals
```

Do not invent a new UI model here. Match Codex first, then simplify only when
the dependency drag is clearly not worth copying.

### 3. Composer

Reference:

```text
codex\codex-rs\tui\src\bottom_pane\
codex\codex-rs\tui\src\chatwidget.rs
codex\codex-rs\tui\src\live_wrap.rs
```

CodeForge target:

```text
src-tauri\src\tui\widgets\composer.rs
```

Goal:

```text
single-line and multiline input
cursor movement
backspace/delete
Home/End
history Up/Down
submit behavior
predictable wrapping
```

This is the part that makes the CLI feel real or fake. Prefer copying more of
Codex's behavior here rather than accumulating local one-off fixes.

### 4. Slash Popup

Reference:

```text
codex\codex-rs\tui\src\app_command.rs
codex\codex-rs\tui\src\bottom_pane\
codex\codex-rs\tui\src\keymap.rs
```

CodeForge target:

```text
src-tauri\src\slash_command.rs
src-tauri\src\tui\widgets\slash_popup.rs
```

Initial commands:

```text
/help
/status
/model
/reason
/goal
/clear
/new
/quit
```

The popup can use local/static command data for the first gate. It does not need
the final agent protocol.

### 5. Transcript Rendering

Reference:

```text
codex\codex-rs\tui\src\history_cell\
codex\codex-rs\tui\src\markdown_render.rs
codex\codex-rs\tui\src\markdown.rs
```

CodeForge target:

```text
src-tauri\src\tui\widgets\transcript.rs
```

Goal for the first gate:

```text
user messages
assistant/system messages
plain markdown-ish wrapping
scrollback
no broken wide-character assumptions where easy to avoid
```

Full markdown fidelity is not required in the first gate. Normal readable output
is required.

### 6. Status/Footer

Reference:

```text
codex\codex-rs\tui\src\status\
codex\codex-rs\tui\src\goal_display.rs
```

CodeForge target:

```text
src-tauri\src\tui\widgets\footer.rs
src-tauri\src\goal_state.rs
```

Goal:

```text
model/provider label
workspace label
reasoning label
goal label when active
busy/idle indicator
```

Usage limits, token accounting, and provider-specific account cards can be
added later.

## Implementation Slice

Do this as one focused slice, not as a broad architecture pass.

1. Make `codeforge` always enter the ratatui path for interactive chat.
2. Replace or heavily revise the current `src-tauri\src\tui` layout against the
   Codex reference.
3. Keep backend calls stubbed or minimal until the TUI is visually acceptable.
4. Support the initial slash commands locally.
5. Keep `run` / `exec` non-interactive behavior working.
6. Build the CLI.
7. Manually launch the binary and verify the first acceptance gate.

## Defer Explicitly

These are important, but they are not the first result:

```text
final tool protocol design
approval request protocol
workspace/apply_patch implementation
VS semantic tools
desktop trace parity
agent streaming parity
Codex app-server-protocol compatibility
old demo cleanup
installer output
```

Do not let any of these block the first normal TUI window.

## Follow-Up Gates

After the first TUI gate passes, continue in this order.

### Gate 2: Real Local Session

```text
interactive prompt sends a task to CodeForge agent runner
assistant response streams or appears in transcript
errors render in transcript without breaking terminal state
trace file is saved as today
```

### Gate 3: Status And Goal State

```text
/status reads actual CodeForge provider/workspace/session state
/goal set/show/clear works
goal state appears in footer
goal state is included in agent context only when intentionally wired
```

### Gate 4: Tool Event Display

```text
tool call start/result/error appears in transcript or side status
trace data is still persisted
credentials are masked
no generic shell tool is introduced
```

### Gate 5: Desktop/CLI Consistency

```text
same backend event shape feeds desktop trace UI and CLI summaries
CLI does not fork a separate hidden tool path
desktop retains richer trace inspection
```

## Build And Validation

For this documentation change:

```text
git diff --check -- docs\codex-desktop-integration-plan.md
```

For code changes in this repo:

```text
build-codeforge-cli.bat
build-release.bat
```

For the TUI gate, manual validation is mandatory:

```text
target\release\codeforge.exe
```

The build passing is not enough. The window must actually open, render normally,
handle input, and exit cleanly.

## Summary

The plan is now outcome-first:

```text
First: make CodeForge launch into a good Codex-like TUI.
Second: wire local session behavior.
Third: add goal/status/tool behavior.
Fourth: converge CLI and desktop traces.
```

If the visible TUI is still bad, the work is not done, even if the code has more
tool abstractions.
