# CodeForge Project Guidance

This repository implements CodeForge Desktop and its local coding-agent runtime.

## Conversation Session Lookup

When the user provides a UUID and says it is a CodeForge or Codex ID, or asks to
analyze a previous answer by ID, inspect the saved conversation before
concluding. Treat "你看看消息" as a request to read the persisted messages.

For CodeForge Desktop IDs, prefer the user's label. A CodeForge ID is a
workspace session ID stored in the local SQLite database:
`%LOCALAPPDATA%\SnowAgentDesktop\codeforge.sqlite3`. Open it read-only. The
session ID maps to `conversation_sessions.id`.

Read these records for a CodeForge session:

1. Session metadata:
   `SELECT id, project_id, prompt, status, created_at, updated_at FROM conversation_sessions WHERE id = ?`
2. Ordered chat messages:
   `SELECT sort_index, id, task_id, role, content, status, created_at FROM conversation_messages WHERE session_id = ? ORDER BY sort_index ASC, created_at ASC, id ASC`
3. Agent run metadata:
   `SELECT run_id, session_id, parent_run_id, agent_name, task_name, provider_id, model_id, status, started_at, ended_at, final_summary FROM agent_runs WHERE session_id = ? ORDER BY started_at ASC`
4. Tool and trace evidence for each run:
   `SELECT * FROM trace_events WHERE run_id = ? ORDER BY step_index ASC, started_at ASC, id ASC`

Use the message `role`, `content`, `status`, `created_at`, and `task_id` as the
main conversation evidence. Use `agent_runs` and `trace_events` to understand
which provider/model ran and what tools or errors influenced the answer. If no
SQLite query capability is available in the current environment, say that the
database cannot be decoded with the available tools and report the exact path
and queries needed instead of guessing.

For Codex Desktop thread IDs, use the Codex thread reader when available. If a
local fallback is needed, look under
`%USERPROFILE%\.codex\sessions\YYYY\MM\DD\rollout-*-<threadId>.jsonl`, then
`%USERPROFILE%\.codex\archived_sessions\...` and
`%USERPROFILE%\.codex\session_index.jsonl`. The rollout JSONL contains
`session_meta`, `response_item`, and `event_msg` entries for thread metadata,
conversation messages, tool calls, and tool results.

Do not send a CodeForge-labeled ID to Codex `read_thread` unless the label is
ambiguous and the CodeForge database has no match. When reporting back, name
which source was read and summarize only the relevant messages and tool
evidence; do not dump large raw transcripts or secrets unless explicitly asked.

## Unreal Project Support

When changing Unreal Engine support, keep these three layers aligned:

1. Runtime prompt guidance should recognize UE workspaces from `*.uproject` or project metadata and require evidence before answering UE, Blueprint, or MCP questions.
2. `/init` should create retrieval docs under `doc/ai-context/` and, for UE projects, also update `.codeforge/codeforge.md` plus `.codeforge/skills/unreal-project/SKILL.md`.
3. The Unreal skill should trigger on UE/Unreal/Blueprint/蓝图/MCP language and force the evidence chain:
   `.uproject` -> `EngineAssociation` -> UE root/source -> project `Config/` and `Saved/Logs/` -> project or engine plugin source -> MCP server/tool state.

Blueprint assets under `Content/**/*.uasset` are binary. Agents must use Unreal MCP/editor tools, generated dumps, logs, or other current evidence for Blueprint graph logic instead of claiming to read the asset as text.
