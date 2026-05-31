# ADR-0011: Tech stack — Rust, eframe/egui, single binary

**Status:** Accepted  
**Date:** 2026-05-31

## Context

The tool is a cross-platform desktop app for Git history cleanup. Key requirements: single binary distribution, fast startup, utilitarian UI, no web runtime overhead.

Options considered:
- **Electron/Tauri + web UI** — heavy runtime (Electron) or still requires web frontend (Tauri). Overkill for a utilitarian tool.
- **Qt/GTK bindings** — complex build, platform-specific quirks, large dependency.
- **eframe/egui** — immediate-mode Rust GUI. Single binary, cross-platform, fast, minimal. Utilitarian aesthetic matches the product vision.

## Decision

- **Language:** Rust (stable toolchain).
- **GUI:** `eframe` / `egui` — immediate-mode, single binary, utilitarian.
- **Native dialogs:** `rfd` — cross-platform file/folder picker dialogs (used for "Open Repository").
- **Git:** user's `git` binary (required) + `git-filter-repo` (optional).
- **Serialization:** `serde` for plan inspection, debugging, and snapshot tests.
- **Errors:** `thiserror` for typed errors in `core`/`gitio`.
- **Time:** `time` crate (`OffsetDateTime`) for commit identities.
- **Async:** not required for v1. Git runs on a worker thread; UI communicates via channel. Kept in `app` only.

## Consequences

- **Single binary** — no installer, no runtime dependencies beyond `git`.
- **Cross-platform** — compiles on Linux, macOS, Windows from the same source.
- **Fast** — native performance, instant startup.
- **Utilitarian UI** — egui's default look matches the product vision. No CSS/theming burden.
- **Trade-off:** egui is less polished than native toolkits for complex UIs. Acceptable for this tool's scope.
