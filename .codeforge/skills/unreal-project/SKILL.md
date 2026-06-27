---
name: unreal-project
description: Work on UE Unreal Engine Blueprint 蓝图 C++ MCP ModelContextProtocol projects by locating project and engine source, logs, config, and live MCP evidence before answering or editing.
triggers: [ue, unreal, blueprint, 蓝图, 虚幻, cpp, c++, mcp, ModelContextProtocol]
---

# Unreal Project

Use this skill for Unreal Engine, UE C++, Blueprint, 蓝图, or Unreal MCP questions.

## Workflow

1. Identify the project:
   - Read the workspace root and locate `*.uproject`.
   - Read the `.uproject` and parse `EngineAssociation`, enabled plugins, and whether `ModelContextProtocol` is enabled.
   - Treat user claims about UE version, plugins, MCP transport, or ports as hypotheses until verified.

2. Locate source evidence:
   - Project C++ usually lives under `Source/`.
   - Project plugin C++ usually lives under `Plugins/*/Source/`.
   - UE engine source lives under `<UE_ROOT>/Engine/Source/`.
   - UE engine plugin source lives under `<UE_ROOT>/Engine/Plugins/**/Source/`.
   - Use `EngineAssociation` to look for common UE roots such as the workspace drive's `\ue\UE_<version>` or `C:\Program Files\Epic Games\UE_<version>`.

3. Handle Blueprint evidence correctly:
   - `Content/**/*.uasset` files are binary packages, not text source.
   - Do not claim to read Blueprint graph logic from raw `.uasset` text.
   - Prefer Unreal MCP/editor tools, generated dumps, asset registry evidence, logs, or project docs when Blueprint internals matter.

4. Verify Unreal MCP claims:
   - Never assume stdio, HTTP, SSE, a default port, or a route from generic MCP knowledge.
   - Inspect the enabled UE plugin source, especially `ModelContextProtocol` source when present.
   - Inspect project config under `Config/` and latest logs under `Saved/Logs/`.
   - Inspect CodeForge MCP config and current MCP server/tool state when available.
   - For port answers, cite source constants/settings, logs, listener state, or a successful protocol handshake.

5. Answer or edit:
   - Cite the concrete files, logs, tools, or current MCP results that support the conclusion.
   - State unknowns as "unknown; verify in code/logs" instead of filling gaps with assumptions.
   - Keep edits scoped to the requested UE project behavior.
