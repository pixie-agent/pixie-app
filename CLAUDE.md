# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Pixie is a **Tauri v2** app whose production target is **Android**. It ships a single **builtin agent engine** that runs the agent loop **in-process** (powered by the `pixie-pi` crate), calling the Anthropic Messages API directly. It does **not** spawn an external CLI and has no subprocess plumbing — the agent runs inside the app's own tokio runtime. A React 19 + TypeScript frontend talks to the Rust backend over the Tauri IPC bridge.

The desktop build (`pnpm tauri dev`) still works and is the primary dev loop, but the shippable product is the Android APK (`src-tauri/gen/android`). `README.md` documents product usage; trust this file when the two disagree.

## Commands

```bash
# Frontend (pnpm is the package manager; Node 18+, Rust stable)
pnpm install
pnpm tauri dev            # desktop dev with hot reload (primary dev loop)
pnpm tauri android dev    # run on an Android device/emulator
pnpm dev                  # Vite frontend only, no Tauri shell
pnpm build                # tsc -b && vite build
pnpm lint                 # ESLint (flat config, eslint.config.js)

# Backend (run from src-tauri/)
cd src-tauri
cargo check               # type-check (host)
cargo ndk check           # type-check the Android target (what actually ships)
cargo fmt
cargo clippy -- -D warnings   # lint (treat warnings as errors per CONTRIBUTING)
cargo test                # all Rust unit tests
cargo test frontmatter    # a single test by name substring

# Bundles
pnpm tauri build                  # desktop bundles for the host OS
pnpm tauri android build          # Android APK / AAB
```

There is **no frontend test runner** — only Rust unit tests (in `src-tauri/src/lib.rs` `#[cfg(test)]`).

Logging is **always on** (debug and release) at `info` level via `tauri-plugin-log`, writing to a rotating `pixie.log` in the app data dir (5 MB rotation, keeps 3, purges files older than 14 days at startup); debug builds also mirror to stdout (the `pnpm tauri dev` terminal). Backend logs are essential when debugging streaming or scheduling — `tail -f` that file. Scheduled runs tag their lines with `[loop]`.

## Architecture

### Engine: builtin, in-process

There is one engine: **`builtin`** (`ENGINE_IDS = &["builtin"]` in `engine/mod.rs`). It does not spawn a process — `BuiltinSession` (`engine/builtin/mod.rs`) wraps a `pixie_pi::AgentSession` and drives the agent loop directly in Rust, mapping `pixie_pi`'s `AgentEvent`s onto pixie's engine-agnostic `NormalizedEvent`s. Configuration comes from the environment: `ANTHROPIC_API_KEY` (required), `ANTHROPIC_BASE_URL` / `ANTHROPIC_MODEL` (optional), with the model overridable per-conversation and per-session.

The agent's tool set is fixed and file-only — `read`, `edit`, `write`, `grep`, `find`, `ls` rooted at the workspace cwd. There is **no shell/bash tool** (it cannot run in the Android sandbox).

The `NormalizedEvent` enum (`engine/mod.rs`) is still the lingua franca: the builtin engine emits these, and `lib.rs::emit_agent_events` translates them into the unified `agent-*` Tauri events the frontend subscribes to. This seam is what would let a future subprocess-based engine coexist, but adding one today means resurrecting the (now-deleted) CLI spawn/parse plumbing — not a small plugin add.

### Agent process model

`send_message` (in `lib.rs`) follows a single path:

- **Builtin session, in-process.** The conversation's `BuiltinSession` is fetched-or-created from `AppState.builtin_sessions` (a `conversation_id → BuiltinSession` map) and `run_turn` is awaited on tokio, streaming `NormalizedEvent`s back through a callback. Subsequent turns reuse the same session so context carries cheaply. `stop_generation` cancels the running turn via the session's `CancellationToken` (`BuiltinSession::cancel`).

There is **no** per-message spawn, PID kill registry, or persistent-CLI stdin pipe — those were the old multi-engine CLI designs and have been removed.

### Streaming → frontend

Backend emits these events (frontend `useChat` routes each by `conversation_id`, so parallel sessions stay independent):
`agent-response` (text delta), `agent-tool` (start/result), `agent-thinking` / `agent-thinking-text`, `agent-usage`, `agent-done`, `agent-error`.

Tool-result text is truncated to `MAX_TOOL_RESULT_CHARS` (8 000) before emission. There is **no interactive permission channel** — the builtin engine auto-approves its fixed tool set, so there is no `agent-permission-request` / `respond_permission` pair.

### Managed state (`AppState`, registered via `.manage()`)

`conversation_engines` (engine binding + per-conversation model override), `workspace` (active working dir), `builtin_sessions` (conversation → in-process agent session). Every `#[tauri::command]` is registered in the single `generate_handler!` list at the bottom of `run()`.

### Persistence (file-backed, not localStorage)

`config.json` (settings + workspaces + engine model config) and `history.jsonl` (one conversation per line) live in the OS app-data dir, written by the `load_app_config` / `save_app_config` / `load_history` / `save_history` Tauri commands. `src/lib/storage.ts` is the frontend's single source of truth: an in-memory module singleton plus a **coalesced, serialized writer** (config debounced 300 ms, history 50 ms) that re-reads live state at flush time so two writers in the same tick never clobber each other. `localStorage` is used **only** for a one-shot migration on first launch.

Scheduled tasks and run history (`TaskRunRecord`) are persisted separately under the app-data dir.

### Scheduler

A tokio task ticks every 60s (`check_and_run_due_tasks`) and fires enabled tasks whose `next_run` is due, via the builtin headless auto-approve path (`run_builtin_task_headless`). Tasks overdue by more than ~5 min are skipped to avoid catch-up bursts.

There is **no system tray**. Closing the window quits the app, so scheduled tasks fire only while Pixie is running.

### Adding a Tauri command (typed end-to-end IPC convention)

Per `CONTRIBUTING.md`: write `#[tauri::command] async fn` in `lib.rs`, register it in the `generate_handler!` list, and use `serde`-derive structs. Mirror the payload as an interface in `src/types.ts`, then call it from a hook with `invoke<T>("command_name", { args })`. The Rust wire format is `snake_case`; `storage.ts` has the wire↔`camelCase` converters (follow that pattern for new persisted types).

## Security posture

The builtin engine runs in-process with a fixed, file-only tool set (`read`/`edit`/`write`/`grep`/`find`/`ls`) rooted at the active workspace, auto-approved (no permission prompts). There is no shell tool. **Only point Pixie at folders you trust the agent to read and modify.** If you change which tools the agent gets or how it is confined to a workspace, treat it as security-sensitive.

## Distribution

Android is the shippable target: build an APK/AAB with `pnpm tauri android build` and distribute the artifact directly. There is **no in-app updater and no auto-update CI** — the Tauri updater plugin, the `/release` skill, `docs/releasing.md`, and the signing-key flow have all been removed. Desktop (`pnpm tauri dev`) is dev-only; `pnpm tauri build` still produces host-OS bundles for local testing but nothing is published.

When bumping the version, keep the 4 version files in sync: `package.json`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, **and** the `pixie` entry in `src-tauri/Cargo.lock`.
