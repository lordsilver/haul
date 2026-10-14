# Agent Instructions

## Engineering approach

- Keep host, queue and transfer logic in `haul-core`, the HTTP API and the web UI in `haul-web`, and only window, tray and webview code in `haul-desktop`. The engine must keep running headless, and the desktop shell must stay thin enough to replace.
- The UI is one set of static files in `crates/haul-web/ui` (vanilla JS modules, no build step) served by the engine. The desktop app loads it from the local server and uses the same HTTP API; do not add Tauri IPC. Keep the Windows 11 (Fluent) look: WinUI theme colors in both themes, Fluent System Icons, Segoe UI Variable.
- Resolvers use public APIs, guest tokens and the user's own accounts only. Do not add captcha solving, wait-timer bypasses, per-download IP rotation or anything else that evades a host's limits. A host that needs a person raises `ResolveError::NeedsUser`; the in-app browser or a pasted link takes it from there.
- Never run, debug or tune host support against links to third-party copyrighted content. Verify hosts with files the user uploaded themselves. Never commit link lists, `queue.json` or `config.toml`.
- While the version is `0.x`, backward compatibility of config keys, state files and the API is not a default requirement; name any break in the release notes. Keep `queue.json` readable in the Python prototype's format unless a break has a concrete benefit.

## Running it

- Every manual run and test uses its own data folder (`--data-dir` or `HAUL_DATA_DIR`) and port (`--port` or `HAUL_PORT`), never the user's real queue.
- To look at the UI or run the desktop app from WSL, use the `haul-ui-check` skill. The desktop app must be forced onto a virtual display, or WSLg puts its windows on the user's desktop.
- A debug build started with `HAUL_UI_DIR=crates/haul-web/ui` reads the UI files from there on every request, so a change to them shows on the next reload without rebuilding. Release builds ignore the variable and serve the embedded copy.

## Pull requests

Changes go through a pull request, squash-merged after CI passes on Linux and Windows; `main` carries one commit per PR. A small change (a typo, a log line) may go straight to `main`.

Before pushing, run `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`. Format Markdown with Prettier.

## Documentation

- `README.md` covers status, installation, configuration and use.
- [docs/hosts.md](docs/hosts.md) lists every host, how it is resolved and what has been verified against the real host; update it with every resolver change.
- [docs/api.md](docs/api.md) describes the HTTP API; update it with every route change.
- [docs/decisions.md](docs/decisions.md) records design decisions with their reasons.

## Releasing

Release notes live only in GitHub releases. They are written once, in the signed tag's message, and the release workflow copies them into the release.

1. Bump `version` in the workspace `Cargo.toml` and the internal crate requirements; build so `Cargo.lock` follows. Commit and push.
2. Wait for CI to pass on that commit.
3. Write the notes: first line `Haul vX.Y.Z`, a blank line, then Markdown with `### Added`, `### Changed`, `### Fixed` as needed, written for users.
4. Tag with `--cleanup=verbatim`, otherwise Git drops the `#` headings: `git tag -s --cleanup=verbatim -F notes.md vX.Y.Z <commit>`, then `git push origin vX.Y.Z`.
5. The release workflow builds the Windows installers and the `haul` server archives, and publishes them with `SHA256SUMS` and build provenance.
