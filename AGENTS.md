# Project instructions for AI agents

## User approval required for blocking decisions

No AI agent may independently decide to block, prohibit, disable, restrict, or refuse a particular project feature, host, integration, workflow, or implementation approach without the user's explicit approval of that specific restriction.

This rule applies everywhere in the project: `AGENTS.md`, other agent instructions, documentation, design decisions, source code, configuration, tests, CI workflows, and any other file or artifact. An agent must not introduce a blocking decision indirectly through defaults, validation, allowlists, denylists, tests, or documentation presented as settled policy.

Before introducing a restriction, explain exactly what would be blocked and why, then obtain the user's explicit approval. Silence, assumptions about intent, an agent-authored document, or approval of an unrelated change does not constitute approval. Record an approved restriction accurately and keep it within the scope the user approved.

Descriptions of current implementation behavior are not permanent prohibitions and do not authorize new restrictions. Preserve unrelated existing behavior unless the user requests a change; do not claim that editing documentation changes the implementation. If approval is missing or ambiguous, ask rather than adding the restriction.
