---
name: haul-release
description: Cut a Haul release — bump the version, write the release notes into an annotated tag, and let the release workflow publish the installers. Use when asked to release, tag or publish a new Haul version.
---

# Releasing Haul

Release notes live only in GitHub releases. They are written once, in the annotated tag's message, and the release workflow copies them into the release.

1. Bump `version` in the workspace `Cargo.toml` and the internal crate requirements; build so `Cargo.lock` follows. Commit and push. Before 1.0, raise the patch number when nothing breaks and the minor number when something does (config keys, state files or the API), so 0.2.8 goes to 0.2.9 or 0.3.0.
2. Wait for CI to pass on that commit (`~/.local/bin/gh run list --commit <sha>` finds the run).
3. Write the notes: first line `Haul vX.Y.Z`, a blank line, then Markdown with `### Added`, `### Changed`, `### Fixed` as needed, written for users. While the version is `0.x`, name every break in config keys, state files or the API.
4. Tag with an unsigned annotated tag and `--cleanup=verbatim`, otherwise Git drops the `#` headings as comments: `git tag -a --cleanup=verbatim -F notes.md vX.Y.Z <commit>`, then `git push origin vX.Y.Z`.
5. The release workflow builds the Windows installers and the `haul` server archives, and publishes them with `SHA256SUMS` and build provenance.
