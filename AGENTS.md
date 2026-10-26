# Project instructions for AI agents

## User approval required for blocking decisions

No AI agent may independently decide to block, prohibit, disable, restrict, or refuse a particular project feature, host, integration, workflow, or implementation approach without the user's explicit approval of that specific restriction.

This rule applies everywhere in the project: `AGENTS.md`, other agent instructions, documentation, design decisions, source code, configuration, tests, CI workflows, and any other file or artifact. An agent must not introduce a blocking decision indirectly through defaults, validation, allowlists, denylists, tests, or documentation presented as settled policy.

Before introducing a restriction, explain exactly what would be blocked and why, then obtain the user's explicit approval. Silence, assumptions about intent, an agent-authored document, or approval of an unrelated change does not constitute approval. Record an approved restriction accurately and keep it within the scope the user approved.

Descriptions of current implementation behavior are not permanent prohibitions and do not authorize new restrictions. Preserve unrelated existing behavior unless the user requests a change; do not claim that editing documentation changes the implementation. If approval is missing or ambiguous, ask rather than adding the restriction.

## Example data

This repository is public. Every example (in code, tests, fixtures, docs, commit messages, pull requests, issues, tags and release notes) uses generic, made-up data that says nothing about what anyone downloads with Haul:

- file and folder names like `archive.part1.rar`, `video.mkv`, `Example/`, `Example.Set`;
- `.example` or `.test` domains (`site.example`, `ads.example`), except for the hosts, wrappers and paste sites Haul supports, which are named because the code works with them;
- IDs and keys that are visibly made up (`abc123`, `AAAAAAAA`).

Nothing is copied from a real page, link or file: no real titles or names, no IDs or keys from live tests, no sites met while testing. Testing against real links locally is fine; describe such runs generically ("a page with six containers"). Check every commit, pull request body, tag message and release note before it leaves the machine.
