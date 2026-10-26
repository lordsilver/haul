# Development

The engine is `crates/haul-core`, the server and its API `crates/haul-web`, and the desktop app `crates/haul-desktop`. Plain `cargo build` and `cargo test` cover the engine and the server; add `--workspace` for the desktop app, which needs WebKitGTK on Linux (see [Installing](install.md)).

## The web UI

The web UI lives in `crates/haul-web/ui` (React, TypeScript, Tailwind and shadcn/ui, built with Vite). Git keeps the built page in `ui/dist`, so building Haul needs no Node.

To change it, with Node 26 and pnpm (`mise install` sets them up): `pnpm install`, then `pnpm dev` for a live page that talks to a Haul already running on port 8765, and `pnpm check && pnpm build` before committing the sources together with `dist`. CI fails when `dist` does not match the sources.

`cargo test` regenerates the page's types from the Rust ones (`ui/src/lib/bindings`).

## Debugging

[Debug mode](debugging.md) streams every signal as it happens, for diagnosing and testing Haul.
