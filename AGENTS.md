# AGENTS.md

Project-local instructions for coding agents working in `D:\code\snowAgents`.

## Required Final Verification

After finishing any code change in this repository, run:

```bat
D:\code\snowAgents\build-release.bat
```

Report whether it passed or failed in the final response. If it fails, include the relevant failure summary and do not claim the change is fully verified.

## Conversation ID Lookup

When the user provides UUIDs and labels them as Codex or CodeForge IDs, for example
`019f077a-0dc2-70b0-9a62-12f7ccf2dcb4 这是codex的id你看看消息` or
`fa210262-35ce-4747-b911-882b99baa253 这是codeforge的id你看看消息`, treat this as
a request to inspect the saved conversation messages for those IDs before answering.

- Codex IDs are Codex Desktop thread IDs. Prefer the Codex thread tool
  (`read_thread`, loaded through `tool_search` if necessary) with the provided
  `threadId`. Read the thread turns/messages and summarize the relevant user,
  assistant, tool-call, and tool-result context. If that tool is unavailable,
  find the local rollout under `%USERPROFILE%\.codex\sessions\YYYY\MM\DD\rollout-*-<threadId>.jsonl`;
  also check `%USERPROFILE%\.codex\archived_sessions\...` and
  `%USERPROFILE%\.codex\session_index.jsonl`. The JSONL records include
  `session_meta` for thread metadata and `response_item` / `event_msg` entries
  for the actual conversation and tool activity.
- CodeForge IDs are CodeForge Desktop workspace session IDs. Read the local
  SQLite database at `%LOCALAPPDATA%\SnowAgentDesktop\codeforge.sqlite3`
  without modifying it. The ID maps to `conversation_sessions.id`; read session
  metadata from `conversation_sessions`, ordered chat messages from
  `conversation_messages WHERE session_id = <id> ORDER BY sort_index`, and
  run/tool details from `agent_runs` plus `trace_events` when needed. The
  message content to inspect is mainly `role`, `content`, `status`,
  `created_at`, and `task_id` (which links a message to an agent run/trace).
- Respect the user's label first: do not send a CodeForge ID to Codex
  `read_thread` unless you are explicitly checking an ambiguous or missing ID.
  When reporting back, say which source was read and summarize the relevant
  messages. Do not dump secrets or large raw transcripts unless explicitly
  requested.

## Scope

Keep changes focused on the user's request. Do not revert unrelated dirty worktree changes.

## MIDAS Gateway Source

The active MIDAS gateway code has been moved to the NAS server. When investigating
or changing gateway behavior, do not assume `D:\work\midas-ai-gateway` is current;
ask for or access the NAS copy first.
