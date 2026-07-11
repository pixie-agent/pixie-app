# Pixie

> A native AI agent workspace with a built-in engine — a general-purpose agent that handles programming, writing, data analysis, notes, and more. Run agents against any folder and watch them work in real time. A built-in **knowledge base** auto-summarizes conversations into searchable, linkable notes. Built with Tauri v2, React, TypeScript, and Rust; ships as an Android app.

![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)
![Tauri](https://img.shields.io/badge/Tauri-v2-blue.svg)
![CI](https://github.com/pixie-agent/pixie-app/actions/workflows/ci.yml/badge.svg)
![Engine](https://img.shields.io/badge/engine-built--in%20Anthropic%20API-orange.svg)
![Platform](https://img.shields.io/badge/platform-Android-blue.svg)

![Pixie](src/assets/hero.svg)

Pixie runs its agent **in-process** — there is no external CLI to install. The built-in engine drives an agent loop directly in Rust (via the `pixie-pi` crate) and calls the Anthropic Messages API, streaming results back as a polished native UI. Configure an `ANTHROPIC_API_KEY` and you're ready.

Use Pixie for programming, office documents, data analysis, news, and writing — wherever an agent can act on files and tools in a folder you choose.

---

## Highlights

- **Built-in engine** — One engine, no install. The agent loop runs inside the app and calls the Anthropic Messages API directly; configure an API key and go.
- **Multi-workspace agents** — Add any number of folders as workspaces. Each becomes the agent's working directory, and many sessions can stream in parallel.
- **Live agent activity** — Streaming markdown with syntax highlighting, real-time tool-call cards, extended-thinking text, and token / duration readouts.
- **Conversation continuity** — Follow-up messages reuse the same in-process session so context carries across turns.
- **Model config** — Override the model and `ANTHROPIC_*` env vars from Settings; each conversation can also pin its own model.
- **Scheduled tasks** — Run prompts on a schedule (daily, weekdays, or every N minutes / hours) headlessly against a workspace. Results appear in the sidebar with notifications.
- **Workspace panel** — A resizable side panel with **Files**, **Preview**, and **Git** for deeper file and version-control access.
- **Knowledge base** — Conversations are summarized to Obsidian-compatible markdown notes with YAML frontmatter. A built-in BM25 search engine (with CJK tokenization via jieba) indexes the vault for fast retrieval. KB context can be injected into agent messages so agents leverage past conversations. Related notes are linked via `[[wiki-links]]`.
- **Dark & light themes**, system prompt, keyboard shortcuts.

---

## Engine

Pixie ships a single **built-in** engine. The agent loop runs in-process (Rust, via `pixie-pi`) and calls the Anthropic Messages API. The agent gets a fixed, file-only tool set — `read`, `edit`, `write`, `grep`, `find`, `ls` rooted at the active workspace (no shell tool). All tool calls are auto-approved within the workspace.

Set `ANTHROPIC_API_KEY` before launching (and optionally `ANTHROPIC_BASE_URL` / `ANTHROPIC_MODEL`). See [Prerequisites](#prerequisites).

---

## Prerequisites

- [Node.js](https://nodejs.org/) v18 or newer
- [pnpm](https://pnpm.io/) (`npm install -g pnpm`)
- A [Rust](https://www.rust-lang.org/tools/install) stable toolchain
- For Android builds: the **Tauri Android prerequisites** (JDK 17, Android Studio / SDK + NDK, and the Rust Android targets — see the [Tauri mobile guide](https://v2.tauri.app/start/prerequisites/))
- An **Anthropic API key** in `ANTHROPIC_API_KEY`

## Installation

```bash
git clone https://github.com/pixie-agent/pixie-app.git
cd pixie-app

pnpm install      # frontend dependencies
```

## Running

```bash
pnpm tauri dev            # desktop dev mode with hot reload (primary dev loop)
pnpm tauri android dev    # run on an Android device/emulator
```

To produce a distributable bundle:

```bash
pnpm tauri build          # desktop bundles for the host OS
pnpm tauri android build  # Android APK / AAB
pnpm tauri android build --split-per-abi --apk  # Smaller per-ABI APKs
```

For GitHub Releases, this repository builds Android artifacts from version tags:

```bash
git tag v0.1.0
git push origin v0.1.0
```

The Android release workflow expects these repository secrets:

- `ANDROID_KEYSTORE_BASE64` — base64-encoded release keystore
- `ANDROID_KEYSTORE_PASSWORD`
- `ANDROID_KEY_ALIAS`
- `ANDROID_KEY_PASSWORD`

> **Note** — The agent acts autonomously within the selected workspace (tools are auto-approved). Only point Pixie at folders you trust the agent to read and modify. See [Security & data](#security--data).

---

## Usage

1. **Add a workspace** — Sidebar → workspace switcher → *Add workspace*, then pick a folder. This is the agent's working directory.
2. **Start an agent** — Type a message and press `Enter`. The first message starts a new session; later messages resume it.
3. **Watch it work** — Tool calls, results, thinking text, and usage update live beneath the reply.
4. **Open the workspace panel** — Toggle the panel in the header for files, diffs, and previews when you need them.
5. **Automate** — **Scheduled Tasks** runs prompts on a timer. Completed runs appear in the sidebar and notify you.

### Keyboard shortcuts

| Action | Shortcut |
| --- | --- |
| New chat | `Ctrl/Cmd + N` |
| Toggle settings | `Ctrl/Cmd + ,` |
| Search knowledge base | `Ctrl/Cmd + K` |
| Send message | `Enter` |
| New line | `Shift + Enter` |
| Stop generation | `Esc` |

---

## Knowledge Base

Pixie includes a local-first knowledge base that turns your conversation history into a searchable vault of notes. It's designed to work with (but does not require) [Obsidian](https://obsidian.md/).

### How it works

1. **Summarize** — After a conversation, click "Summarize" to write it as a markdown note to `<vault>/Pixie/<slug>-<convId>.md`. The note includes YAML frontmatter (title, conversation_id, workspace, engine, tags, created) and the full transcript.
2. **Index** — A Rust-based inverted-index search engine scans all `*.md` files in the vault's `Pixie/` directory. It builds a BM25 scoring index with CJK tokenization (jieba-rs) for Chinese, Japanese, and Korean text.
3. **Search** — Press `Ctrl/Cmd + K` to open the search palette. Type a query (minimum 2 characters) and get debounced BM25-ranked results with snippets, tags, and dates. Press `Enter` to open a note in Obsidian, or copy content inline.
4. **Inject** — Toggle the knowledge base (database cylinder icon in the input bar) to inject relevant snippets from search results into agent messages as context. This lets agents reference past work without re-explaining.
5. **Related links** — When a new note is written, the summarizer searches for related notes via BM25 and appends `[[wiki-link]]` references at the bottom, creating a navigable knowledge graph.

### Setup

- **Configure vault path** — Open Settings (`Ctrl/Cmd + ,`) and set your Obsidian vault path. If unset, Pixie stores the default vault under the app data directory.
- **Backfill existing conversations** — Use the "Backfill" button in Settings to summarize all past conversations into notes.
- **Obsidian integration is optional** — The KB works entirely within Pixie. Obsidian is only needed for external viewing/editing.

---

## Architecture

Pixie is a Tauri v2 app: a Rust backend that owns the in-process agent loop, plus a React frontend over the IPC bridge.

```
┌───────────────────────────────────────────────────────┐
│  Frontend  ·  React + TypeScript + Tailwind CSS        │
│                                                        │
│  hooks (useChat / useScheduledTasks)                   │
│      │  invoke()  ────────►  Tauri commands            │
│      │  listen()  ◄────────  Tauri events (streaming)  │
└──────────────────────────┬────────────────────────────┘
                           │  Tauri IPC bridge
┌──────────────────────────┴────────────────────────────┐
│  Backend  ·  Rust (tokio)                              │
│                                                        │
│  Chat         send_message / stop_generation           │
│  Engine       builtin session (in-process agent loop)  │
│  Workspaces   select / set_active / list_directory     │
│  KB           search_kb / index_kb / summarize / …     │
│  Git / Files / Schedules                               │
│                                                        │
│  Events: agent-response · agent-tool · agent-done · …  │
└──────────────────────────┬────────────────────────────┘
                           │  in-process (no subprocess)
┌──────────────────────────┴────────────────────────────┐
│  engine/builtin/  ·  pixie_pi::AgentSession            │
│    BuiltinSession   wraps the agent loop               │
│    tools            read / edit / write / grep / find / ls │
│    mod.rs           NormalizedEvent mapping             │
└───────────────────────────────────────────────────────┘
```

How a message flows:

- The frontend calls `invoke("send_message", { conversationId, … })`. The backend fetches-or-creates a `BuiltinSession` for that conversation and runs the agent loop **in-process** on tokio, returning events as they arrive.
- Each agent event is mapped to a **normalized event** (text delta, tool start/result, usage, done) and emitted as a unified `agent-*` Tauri event.
- `useChat` routes updates by `conversation_id` so parallel sessions stay independent.
- `stop_generation` cancels the running turn via the session's cancellation token.

**Where state lives:** conversations (including per-session model), workspaces, theme, and model config live in `config.json` / `history.jsonl` under the OS app-data dir (written through a coalesced serializer in `src/lib/storage.ts`). Scheduled tasks and run history are persisted alongside them.

---

## Project structure

```
pixie/
├── src/                         # Frontend (React + TypeScript)
│   ├── components/              # ChatView, Sidebar, Settings, RightPanel, …
│   ├── hooks/                   # useChat, useScheduledTasks, useIsMobile, …
│   ├── i18n/                    # zh / en / ja locales
│   ├── App.tsx
│   └── types.ts
├── src-tauri/
│   ├── src/
│   │   ├── engine/              # Agent backend
│   │   │   ├── mod.rs           # NormalizedEvent, ENGINE_IDS, dispatch
│   │   │   ├── builtin/mod.rs   # BuiltinSession (pixie_pi::AgentSession)
│   │   │   └── shared.rs        # Engine status helpers
│   │   ├── search/              # Knowledge base search engine
│   │   │   ├── mod.rs           # Index lifecycle, Tauri commands
│   │   │   ├── bm25.rs          # BM25 scoring + jieba-rs CJK tokenizer
│   │   │   ├── index.rs         # Inverted-index search engine
│   │   │   └── parser.rs        # Obsidian YAML frontmatter parser
│   │   ├── summarizer.rs        # Conversation → KB note writer
│   │   └── lib.rs               # Tauri commands, scheduler, AppState
│   ├── gen/android/             # Tauri-generated Android project
│   └── tauri.conf.json
├── package.json
└── vite.config.ts
```

---

## Development

```bash
pnpm dev                  # Vite dev server only (no Tauri shell)
pnpm tauri dev            # Full desktop app with hot reload
pnpm tauri android build --split-per-abi --apk

pnpm lint                 # ESLint

cd src-tauri
cargo check               # Type-check Rust (host)
cargo clippy              # Lint Rust
cargo test                # Unit tests
```

### Key technologies

| Layer | Technology |
| --- | --- |
| App framework | Tauri v2 |
| Frontend | React 19, TypeScript |
| Styling | Tailwind CSS 4 |
| Build tool | Vite |
| Backend | Rust, tokio |
| Agent loop | `pixie-pi` (in-process, Anthropic Messages API) |
| Markdown | react-markdown + remark-gfm |
| Scheduling | chrono |
| CJK search | jieba-rs (Chinese word segmentation) |
| BM25 search | custom inverted-index engine |

### Configuration

Open **Settings** (`Ctrl/Cmd + ,`):

- **Engine** — readiness check (pings the model) and version.
- **Model configuration** — env overrides for the builtin engine (`ANTHROPIC_API_KEY`, `ANTHROPIC_BASE_URL`, `ANTHROPIC_MODEL`, …).
- **Knowledge base** — Obsidian vault path, backfill existing conversations, and index rebuild.
- **System prompt** — optional prompt for agent sessions.
- **Theme** — dark or light.

---

## Security & data

- The agent runs in-process with auto-approved, file-only tools (`read`/`edit`/`write`/`grep`/`find`/`ls`) within the active workspace. Only add workspaces you trust the agent to operate on.
- Chat content, workspaces, and settings stay local (app-data dir). The only network traffic is to the Anthropic API via the configured key/base URL.

---

## Troubleshooting

**Engine not ready** — Set `ANTHROPIC_API_KEY` (and optionally `ANTHROPIC_BASE_URL`) and restart. Use Settings → *Re-detect* to ping the model.

**Scheduled task didn't fire** — Pixie must be running (there is no tray; closing the app quits it). Tasks overdue by more than 5 minutes are skipped to avoid catch-up bursts. Use *Run now* to test.

**Knowledge base search returns no results** — Ensure the vault path in Settings points to a valid directory containing `.md` files under a `Pixie/` subfolder. Use "Rebuild Index" in Settings if the index is stale. Minimum query length is 2 characters.

**KB notes not appearing** — Summarize a conversation first. Notes are written to `<vault>/Pixie/`. If you moved or renamed the vault, update the path in Settings and rebuild the index.

**Build errors** — `rustup update`, `cd src-tauri && cargo clean`, `rm -rf node_modules && pnpm install`.

---

## Contributing

Contributions are welcome:

1. Fork the repo and create a feature branch.
2. Rust: `cargo fmt` / `cargo clippy`. Frontend: `pnpm lint`.
3. Keep Tauri commands typed end-to-end (Rust ↔ `src/types.ts`).
4. Open a pull request describing the change.

See `CONTRIBUTING.md` for details.

## License

Released under the [MIT License](LICENSE).
