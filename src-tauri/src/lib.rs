mod engine;
mod search;
mod summarizer;

use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone, Utc};
use engine::builtin::{init_builtin_sessions, BuiltinSession, BuiltinSessionMap};
use engine::{
    bind_conversation_engine, check_all_engines, conversation_engine_id,
    init_conversation_engine_map, normalize_engine_id, set_conversation_model,
    ConversationEngineMap, EngineStatus, NormalizedEvent,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// Android folder picker plugin state
// ---------------------------------------------------------------------------

// NOTE: a native Android SAF folder-picker (FolderPickerPlugin.kt + Rust glue)
// was attempted but had unresolved Tauri plugin generic type errors, so it was
// removed. A default workspace is auto-created from `get_default_workspace_path`
// on Android (the app data dir), so chat works without an explicit picker. The
// pick_folder/select_workspace/pick_files commands return Ok(None) on Android.

// ---------------------------------------------------------------------------
// Data types shared with the frontend
// ---------------------------------------------------------------------------

/// Frontend app settings + workspace/session state, persisted as `config.json`.
/// Each field is optional/defaults so older files (or partial writes) still load.
/// `engine_model_configs` carries per-engine env overrides (incl. API keys) as an
/// opaque JSON blob — the frontend owns that schema.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    #[serde(default)]
    pub theme: Option<String>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub default_engine: Option<String>,
    #[serde(default)]
    pub engine_model_configs: serde_json::Value,
    #[serde(default)]
    pub workspaces: Vec<serde_json::Value>,
    #[serde(default)]
    pub active_workspace_id: Option<String>,
    /// Engine ids that have previously passed a readiness probe (logged in +
    /// working). The frontend uses this to skip a billable ping on launch for
    /// returning users, and removes an id when a later probe fails.
    #[serde(default)]
    pub known_ready_engines: Vec<String>,
    /// Path to the Obsidian vault directory. If unset, conversation summaries
    /// are skipped silently (no fallback to a hidden data-dir folder).
    #[serde(default)]
    pub vault_path: Option<String>,
}

/// One line of `history.jsonl`: a conversation (full, with messages) and the
/// workspace path it belongs to. The conversation is an opaque JSON value so the
/// frontend stays the single source of truth for the message schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub workspace_id: String,
    pub conversation: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineStatusResponse {
    pub id: String,
    pub display_name: String,
    pub available: bool,
    pub version: Option<String>,
    pub path: Option<String>,
    pub error: Option<String>,
    #[serde(default)]
    pub auth_state: engine::AuthState,
    #[serde(default)]
    pub probe_error: Option<String>,
}

impl From<EngineStatus> for EngineStatusResponse {
    fn from(s: EngineStatus) -> Self {
        Self {
            id: s.id,
            display_name: s.display_name,
            available: s.available,
            version: s.version,
            path: s.path,
            error: s.error,
            auth_state: s.auth_state,
            probe_error: s.probe_error,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseChunk {
    pub conversation_id: String,
    pub content: String,
    pub event_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseDone {
    pub conversation_id: String,
    pub full_text: String,
}

/// Live tool-use activity (a tool starting or its result arriving) for real-time progress.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseTool {
    pub conversation_id: String,
    pub tool_use_id: String,
    /// "start" when a tool is invoked, "result" when its output arrives.
    pub kind: String,
    pub name: Option<String>,
    /// Raw tool `input` as a JSON string (present on "start").
    pub input: Option<String>,
    /// Tool result text (present on "result").
    pub content: Option<String>,
    pub is_error: bool,
}

/// Token / cost usage for a stage. `kind` = "turn" (per assistant message, accumulate for a
/// live total) or "final" (authoritative totals + cost/duration/turns from the result event).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseUsage {
    pub conversation_id: String,
    pub kind: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cost_usd: Option<f64>,
    pub duration_ms: Option<u64>,
    pub num_turns: Option<u64>,
    pub model: Option<String>,
    pub stop_reason: Option<String>,
}

/// Live thinking-budget token estimate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseError {
    pub conversation_id: String,
    pub error: String,
}

/// A chunk of the model's private reasoning (extended thinking) text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseThinkingText {
    pub conversation_id: String,
    pub content: String,
}

// ---------------------------------------------------------------------------
// Scheduled tasks
// ---------------------------------------------------------------------------

/// A user-chosen schedule preset. Tagged so the frontend can build the literal
/// `{ type: "daily_time", hour, minute }` shape directly.
/// Fields are authored in the user's LOCAL time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScheduleSpec {
    /// Daily at HH:MM (24h, local).
    DailyTime { hour: u32, minute: u32 },
    /// Every N minutes.
    EveryNMinutes { minutes: u32 },
    /// Every N hours.
    EveryNHours { hours: u32 },
    /// Weekdays (Mon–Fri) at HH:MM.
    WeekdaysTime { hour: u32, minute: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledTask {
    pub id: String,
    pub name: String,
    /// Workspace folder path (WorkspaceState.path). Used as the builtin agent's working directory.
    pub workspace: String,
    pub prompt: String,
    pub schedule: ScheduleSpec,
    pub enabled: bool,
    /// Engine to use for this task. Defaults to "builtin".
    #[serde(default = "default_task_engine")]
    pub engine: String,
    /// ISO-8601 (UTC) of the run we are currently waiting for. None when disabled.
    #[serde(default)]
    pub next_run: Option<String>,
    /// ISO-8601 (UTC) of the last successful fire, for display.
    #[serde(default)]
    pub last_run: Option<String>,
}

/// A completed (or failed) scheduled-task execution, persisted as the durable
/// "full AI execution record". The Rust `Conversation` struct intentionally does
/// not carry messages, so the result needs its own home on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRunRecord {
    /// Same id as the conversation_id used to run the task.
    pub id: String,
    pub task_id: String,
    pub task_name: String,
    pub workspace: String,
    pub prompt: String,
    /// Final assistant text (empty on error).
    pub result: String,
    /// "ok" | "error"
    pub status: String,
    /// ISO-8601 (UTC)
    pub started_at: String,
    /// ISO-8601 (UTC)
    pub finished_at: String,
}

fn default_task_engine() -> String {
    "builtin".to_string()
}

// ---------------------------------------------------------------------------
// Application state
// ---------------------------------------------------------------------------

pub struct AppState {
    /// conversation_id → engine binding + per-conversation model override.
    conversation_engines: ConversationEngineMap,
    /// User-selected workspace directory
    workspace: Arc<Mutex<Option<String>>>,
    /// Builtin engine sessions (in-process agent loop)
    builtin_sessions: BuiltinSessionMap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
}

/// A Claude skill discovered on disk (user- or project-level), surfaced so the
/// input bar can offer a `/skill-name` picker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillEntry {
    /// Slash-invocable name, e.g. "skill-creator".
    pub name: String,
    /// One-line description (from SKILL.md frontmatter, or derived from the body).
    pub description: String,
    /// "user" (from ~/.claude) or "project" (from <workspace>/.claude).
    pub source: String,
    /// What gets inserted into the textarea, e.g. "/skill-creator ".
    pub invocation: String,
}

// ---------------------------------------------------------------------------
// Agent event emission (engine-agnostic)
// ---------------------------------------------------------------------------

const MAX_TOOL_RESULT_CHARS: usize = engine::MAX_TOOL_RESULT_CHARS;

fn truncate_for_ui(text: &str, max: usize) -> String {
    engine::truncate_text(text, max)
}

fn usage_is_empty(u: &engine::UsageInfo) -> bool {
    u.input_tokens == 0
        && u.output_tokens == 0
        && u.cache_read_tokens == 0
        && u.cache_creation_tokens == 0
        && u.cost_usd.is_none()
        && u.duration_ms.is_none()
        && u.num_turns.is_none()
}

fn emit_agent_events(app: &AppHandle, conversation_id: &str, events: &[NormalizedEvent]) {
    for evt in events {
        if let Some(text) = evt.streaming_text() {
            let event_type = evt
                .streaming_text_event_type()
                .unwrap_or("delta")
                .to_string();
            let _ = app.emit(
                "agent-response",
                ResponseChunk {
                    conversation_id: conversation_id.to_string(),
                    content: text,
                    event_type,
                },
            );
        }

        if let Some(thinking) = evt.streaming_thinking() {
            let _ = app.emit(
                "agent-thinking-text",
                ResponseThinkingText {
                    conversation_id: conversation_id.to_string(),
                    content: thinking,
                },
            );
        }

        if let Some(te) = evt.tool_event() {
            let (kind, name, input, content, is_error) = match &te.kind {
                engine::ToolEventKind::Start { name, input } => {
                    ("start", name.clone(), input.clone(), None, false)
                }
                engine::ToolEventKind::Result { content, is_error } => {
                    let truncated = content
                        .as_ref()
                        .map(|c| truncate_for_ui(c, MAX_TOOL_RESULT_CHARS));
                    ("result", None, None, truncated, *is_error)
                }
            };
            let _ = app.emit(
                "agent-tool",
                ResponseTool {
                    conversation_id: conversation_id.to_string(),
                    tool_use_id: te.id.clone(),
                    kind: kind.to_string(),
                    name,
                    input,
                    content,
                    is_error,
                },
            );
        }

        if let Some(u) = evt.usage() {
            if u.kind == "turn" && usage_is_empty(u) {
                continue;
            }
            let _ = app.emit(
                "agent-usage",
                ResponseUsage {
                    conversation_id: conversation_id.to_string(),
                    kind: u.kind.to_string(),
                    input_tokens: u.input_tokens,
                    output_tokens: u.output_tokens,
                    cache_read_tokens: u.cache_read_tokens,
                    cache_creation_tokens: u.cache_creation_tokens,
                    cost_usd: u.cost_usd,
                    duration_ms: u.duration_ms,
                    num_turns: u.num_turns,
                    model: u.model.clone(),
                    stop_reason: u.stop_reason.clone(),
                },
            );
        }

        if let NormalizedEvent::Error { message } = evt {
            let _ = app.emit(
                "agent-error",
                ResponseError {
                    conversation_id: conversation_id.to_string(),
                    error: message.clone(),
                },
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn send_message(
    message: String,
    conversation_id: String,
    engine: Option<String>,
    model: Option<String>,
    images: Option<Vec<String>>,
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<Option<String>, String> {
    let engine_id = match engine.as_deref() {
        Some(e) => normalize_engine_id(e).map_err(|e| e.to_string())?,
        None => {
            if let Some(existing) =
                conversation_engine_id(&state.conversation_engines, &conversation_id).await
            {
                normalize_engine_id(&existing).map_err(|e| e.to_string())?
            } else {
                "builtin"
            }
        }
    };

    bind_conversation_engine(&state.conversation_engines, &conversation_id, engine_id).await;
    set_conversation_model(&state.conversation_engines, &conversation_id, model.clone()).await;

    log::info!(
        "[send_message] start: conv_id={}, engine={}, model={:?}, msg_len={}",
        conversation_id,
        engine_id,
        model,
        message.len()
    );

    let workspace = state.workspace.lock().await.clone();
    let images_owned = images.unwrap_or_default();

    // --- Builtin engine path ---
    if engine_id == "builtin" {
        let builtin_sessions = state.builtin_sessions.clone();
        let mut sessions = builtin_sessions.lock().await;

        let needs_create = !sessions.contains_key(&conversation_id);
        if needs_create {
            let api_key = engine::builtin::get_api_key();
            if api_key.is_empty() {
                return Err("No ANTHROPIC_API_KEY configured for builtin engine".to_string());
            }
            let base_url = engine::builtin::get_base_url();
            let cwd = workspace.clone().unwrap_or_else(|| {
                get_data_dir(&app)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| ".".to_string())
            });
            let session = BuiltinSession::new(
                model.as_deref(),
                None,
                &cwd,
                &api_key,
                base_url.as_deref(),
            );
            sessions.insert(conversation_id.clone(), session);
        }

        let session = sessions.get_mut(&conversation_id).unwrap();
        let app_h = app.clone();
        let conv_c = conversation_id.clone();
        let result = session
            .run_turn(&message, &images_owned, |evt| {
                emit_agent_events(&app_h, &conv_c, &[evt]);
            })
            .await;

        drop(sessions);

        match result {
            Ok((final_text, had_error)) => {
                log::info!(
                    "[send_message] builtin done, total_len={}, had_error={}",
                    final_text.len(),
                    had_error
                );
                if !had_error {
                    let _ = app.emit(
                        "agent-done",
                        ResponseDone {
                            conversation_id: conversation_id.clone(),
                            full_text: final_text.clone(),
                        },
                    );
                    // Return the full text as the command result so Android
                    // (where emit push may not work in dev mode) can still
                    // display the response.
                    return Ok(Some(final_text));
                }
                // Error turn — still return what we have so the user sees it.
                return Ok(Some(final_text));
            }
            Err(e) => {
                log::error!("[send_message] builtin error: {}", e);
                let _ = app.emit(
                    "agent-error",
                    ResponseError {
                        conversation_id: conversation_id.clone(),
                        error: e.to_string(),
                    },
                );
                return Err(e.to_string());
            }
        }
    }

    Ok(None)
}

#[tauri::command]
fn list_directory(path: String) -> Result<Vec<FileEntry>, String> {
    let entries =
        std::fs::read_dir(&path).map_err(|e| format!("Failed to read directory: {}", e))?;
    let mut files: Vec<FileEntry> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
        let metadata = entry
            .metadata()
            .map_err(|e| format!("Failed to read metadata: {}", e))?;
        files.push(FileEntry {
            name: entry.file_name().to_string_lossy().to_string(),
            path: entry.path().to_string_lossy().to_string(),
            is_dir: metadata.is_dir(),
            size: metadata.len(),
        });
    }
    files.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(files)
}

#[tauri::command]
fn read_file_content(path: String) -> Result<String, String> {
    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("Failed to read file: {}", e))?;
    if content.len() > 500_000 {
        Ok(content[..500_000].to_string() + "\n\n... (truncated)")
    } else {
        Ok(content)
    }
}

// ---------------------------------------------------------------------------
// Skill discovery (user-level ~/.claude/skills + project-level .claude/skills)
// ---------------------------------------------------------------------------

/// Parse leading `---` YAML frontmatter from a SKILL.md body.
/// Returns `(name, description, body)`. When there is no frontmatter, the
/// whole text is returned as the body and both options are `None`. Line-based
/// (no byte-offset math) so it is safe on arbitrary UTF-8.
fn parse_frontmatter(text: &str) -> (Option<String>, Option<String>, String) {
    let mut lines = text.lines();
    let first = match lines.next() {
        Some(l) => l,
        None => return (None, None, String::new()),
    };
    if first.trim() != "---" {
        // No frontmatter: re-include the first line so description derivation
        // can read the document's first heading.
        let mut body = String::from(first);
        for l in lines {
            body.push('\n');
            body.push_str(l);
        }
        return (None, None, body);
    }

    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut closed = false;
    let mut body = String::new();
    for line in lines {
        if !closed {
            if line.trim() == "---" {
                closed = true;
                continue;
            }
            if let Some(v) = line.strip_prefix("name:") {
                if name.is_none() {
                    name = Some(strip_scalar(v));
                }
            } else if let Some(v) = line.strip_prefix("description:") {
                if description.is_none() {
                    description = Some(strip_scalar(v));
                }
            }
        } else if !body.is_empty() {
            body.push('\n');
            body.push_str(line);
        } else {
            body.push_str(line);
        }
    }

    if closed {
        (name, description, body)
    } else {
        // Opening fence with no closing fence: treat as no frontmatter.
        (None, None, body)
    }
}

/// Trim a YAML scalar value, stripping surrounding quotes.
fn strip_scalar(v: &str) -> String {
    v.trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string()
}

/// Derive a one-line description from the first meaningful markdown line.
fn derive_description(body: &str) -> String {
    for line in body.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let cleaned = t.trim_start_matches('#').trim().replace("**", "");
        let cleaned = cleaned.trim();
        if !cleaned.is_empty() {
            return truncate_str(cleaned, 160);
        }
    }
    String::new()
}

/// Truncate to `n` chars (UTF-8 safe) with an ellipsis.
fn truncate_str(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n).collect();
    out.push('…');
    out
}

/// Build a SkillEntry from a `<skills>/<name>/SKILL.md` path.
/// The skill name falls back to the parent directory's stem when the
/// frontmatter omits `name`.
fn skill_entry_from_file(skill_md: &std::path::Path, source: &str) -> Option<SkillEntry> {
    let bytes = fs::read(skill_md).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let (fm_name, fm_desc, body) = parse_frontmatter(&text);

    let name = fm_name.or_else(|| {
        skill_md
            .parent()
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .map(String::from)
    })?;
    let name = name.trim().to_string();
    if name.is_empty() {
        return None;
    }

    Some(SkillEntry {
        name: name.clone(),
        description: truncate_str(&fm_desc.unwrap_or_else(|| derive_description(&body)), 200),
        source: source.to_string(),
        invocation: format!("/{} ", name),
    })
}

/// Scan `<root>/skills/*/SKILL.md` and append any entries found.
fn scan_skills(root: &std::path::Path, source: &str, out: &mut Vec<SkillEntry>) {
    let Ok(entries) = fs::read_dir(root.join("skills")) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let skill_md = dir.join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        if let Some(skill) = skill_entry_from_file(&skill_md, source) {
            out.push(skill);
        }
    }
}

/// Recursively walk a plugin tree and collect every `SKILL.md` found.
/// Used for `~/.claude/plugins`, whose layout is
/// `marketplaces/<mp>/(plugins|external_plugins)/<plugin>/skills/<skill>/SKILL.md`.
fn scan_plugin_skills(root: &std::path::Path, out: &mut Vec<SkillEntry>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            // Skip noisy / irrelevant trees (.git, .github, node_modules, ...).
            if name == "node_modules" || name.starts_with('.') {
                continue;
            }
            scan_plugin_skills(&path, out);
        } else if path.is_file() && name == "SKILL.md" {
            if let Some(skill) = skill_entry_from_file(&path, "plugin") {
                out.push(skill);
            }
        }
    }
}

/// List all discoverable skills: user-level (`~/.claude/skills`),
/// project-level (`<workspace>/.claude/skills`) and plugin skills
/// (`~/.claude/plugins/**/SKILL.md`). When names collide, project shadows
/// user, which shadows plugin.
#[tauri::command]
fn list_skills(workspace: Option<String>) -> Result<Vec<SkillEntry>, String> {
    let mut entries: Vec<SkillEntry> = Vec::new();

    if let Ok(home) = std::env::var("HOME") {
        let claude_root = std::path::Path::new(&home).join(".claude");
        scan_skills(&claude_root, "user", &mut entries);
        scan_plugin_skills(&claude_root.join("plugins"), &mut entries);
    }
    if let Some(ws) = workspace {
        let ws = ws.trim();
        if !ws.is_empty() {
            let project_root = std::path::Path::new(ws).join(".claude");
            scan_skills(&project_root, "project", &mut entries);
        }
    }

    // Rank: project (0) > user (1) > plugin (2); then alphabetical by name.
    // dedup_by keeps the first of each name, so a project skill shadows a
    // same-named user skill, which shadows a plugin one.
    let rank = |s: &str| -> i32 {
        match s {
            "project" => 0,
            "user" => 1,
            _ => 2,
        }
    };
    entries.sort_by(|a, b| {
        rank(&a.source)
            .cmp(&rank(&b.source))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    entries.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));

    Ok(entries)
}

#[tauri::command]
async fn get_default_workspace_path(app: AppHandle) -> Result<String, String> {
    // A user-configured override (Settings) wins.
    if let Some(custom) = load_default_workspace_override(&app) {
        let dir = std::path::Path::new(&custom);
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("failed to create default workspace '{custom}': {e}"))?;
        return Ok(custom);
    }
    // On Android there is no user HOME, so default the workspace to the app
    // data dir. The frontend's `ensureDefault` then auto-creates a workspace
    // from this path, unblocking chat without a folder picker.
    let dir = get_data_dir(&app)?;
    let _ = std::fs::create_dir_all(&dir);
    Ok(dir.to_string_lossy().to_string())
}

/// Configure the default working directory from Settings. `None` (or an empty
/// string) clears the override so the default falls back to `~/.pixie`. The
/// chosen folder is created if needed so it is usable as an agent CWD. This is
/// config-only: it does not move or modify existing workspaces.
#[tauri::command]
async fn set_default_workspace_path(path: Option<String>, app: AppHandle) -> Result<(), String> {
    match path {
        Some(raw) => {
            let trimmed = raw.trim().to_string();
            if trimmed.is_empty() {
                persist_default_workspace_override(&app, &None);
                return Ok(());
            }
            std::fs::create_dir_all(&trimmed)
                .map_err(|e| format!("cannot use '{trimmed}' as default workspace: {e}"))?;
            persist_default_workspace_override(&app, &Some(trimmed));
        }
        None => {
            persist_default_workspace_override(&app, &None);
        }
    }
    Ok(())
}

/// Open a native folder picker (works on Android via tauri-plugin-dialog).
#[tauri::command]
async fn pick_folder(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let folder = app.dialog().file().blocking_pick_file();
    Ok(folder.map(|p| p.to_string()))
}

#[tauri::command]
async fn set_active_workspace(
    path: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let mut workspace = state.workspace.lock().await;
    *workspace = Some(path);
    Ok(())
}

#[tauri::command]
async fn set_engine_model_config(
    engine: String,
    config: HashMap<String, String>,
) -> Result<(), String> {
    engine::set_engine_model_config(&engine, config);
    Ok(())
}

/// Called by the frontend when the user changes the per-conversation model
/// override in the InputBar dropdown. Updates the backend model state and
/// kills any existing persistent session so the next `send_message` will
/// respawn it with the new model.
#[tauri::command]
async fn update_conversation_model(
    conversation_id: String,
    model: Option<String>,
    engine: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let engine_id = match engine.as_deref() {
        Some(e) => engine::normalize_engine_id(e).map_err(|e| e.to_string())?,
        None => &conversation_engine_id(&state.conversation_engines, &conversation_id)
            .await
            .unwrap_or_else(|| "builtin".to_string()),
    };

    // Update the engine binding first (ensures entry exists).
    bind_conversation_engine(&state.conversation_engines, &conversation_id, engine_id).await;

    // Store the model override.
    engine::set_conversation_model(&state.conversation_engines, &conversation_id, model.clone())
        .await;

    log::info!(
        "[set_conversation_model] conv_id={}, engine={}, model={:?}",
        conversation_id,
        engine_id,
        model
    );

    Ok(())
}

#[tauri::command]
async fn stop_generation(
    conversation_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    // Builtin engine: cancel the in-process agent loop for this conversation.
    let mut sessions = state.builtin_sessions.lock().await;
    if let Some(session) = sessions.get_mut(&conversation_id) {
        log::info!(
            "[stop_generation] cancelling builtin session for conv {}",
            conversation_id
        );
        session.cancel();
        sessions.remove(&conversation_id);
    }
    Ok(())
}

#[tauri::command]
async fn select_workspace(
    _app: AppHandle,
    _state: tauri::State<'_, AppState>,
) -> Result<Option<String>, String> {
    // No native folder picker on Android; a default workspace is auto-created,
    // so chat works without explicitly selecting one.
    Ok(None)
}

/// Open a native multi-file picker (works on Android via tauri-plugin-dialog).
#[tauri::command]
async fn pick_files(app: AppHandle) -> Result<Option<Vec<String>>, String> {
    use tauri_plugin_dialog::DialogExt;
    let files = app.dialog().file().blocking_pick_files();
    Ok(files.map(|paths| paths.iter().map(|p| p.to_string()).collect()))
}

/// Persist a pasted screenshot/image to disk and return its absolute path.
///
/// The webview can only hand us clipboard image data as bytes over IPC; to keep
/// that compact the frontend base64-encodes the Blob. We decode here, write it
/// under `<data_dir>/pasted-images/`, and hand the path back so the composer can
/// stage it as an attachment (→ `@mention`) exactly like a dragged-in file.
/// The path lives outside the workspace, so it is sent as an absolute `@mention`
/// (engines run with permissions bypassed, so reading it is allowed).
#[tauri::command]
async fn save_pasted_image(app: AppHandle, data: String, ext: String) -> Result<String, String> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let bytes = STANDARD
        .decode(data.as_bytes())
        .map_err(|e| format!("Invalid base64 image data: {e}"))?;
    if bytes.is_empty() {
        return Err("Empty image data".to_string());
    }
    let dir = get_data_dir(&app)?.join("pasted-images");
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create pasted-images dir: {e}"))?;
    // Monotonic-enough filename: no Math.random on the JS side either, and this
    // single millisecond is unique enough for human paste cadence.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("System clock error: {e}"))?
        .as_millis();
    let safe_ext = {
        let e = ext.trim().trim_start_matches('.').to_lowercase();
        if e.is_empty() {
            "png".to_string()
        } else {
            e
        }
    };
    let path = dir.join(format!("pasted-{now}.{safe_ext}"));
    fs::write(&path, &bytes).map_err(|e| format!("Failed to write pasted image: {e}"))?;
    Ok(path.to_string_lossy().to_string())
}

/// Persist a user-selected file (from the Android file picker / webview
/// `<input type=file>`) to disk and return its absolute path.
///
/// On Android, `tauri-plugin-dialog`'s `pick_files` returns `content://`
/// URIs that the agent's `read` tool cannot open with `std::fs` (they are
/// SAF virtual URIs, not real filesystem paths). To make uploaded files
/// readable by the agent, the frontend instead uses a hidden `<input
/// type=file>`, reads the chosen File as base64, and ships it here. We decode
/// and write it under `<data_dir>/uploads/` (preserving the original
/// filename when safe) and return the real absolute path, which is then
/// staged as an attachment / `@mention` exactly like a pasted image.
#[tauri::command]
async fn save_uploaded_file(
    app: AppHandle,
    data: String,
    filename: String,
) -> Result<String, String> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let bytes = STANDARD
        .decode(data.as_bytes())
        .map_err(|e| format!("Invalid base64 file data: {e}"))?;
    if bytes.is_empty() {
        return Err("Empty file data".to_string());
    }
    let dir = get_data_dir(&app)?.join("uploads");
    fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create uploads dir: {e}"))?;

    // Sanitize the client-supplied filename: keep the base name, strip path
    // separators, drop anything not alphanumeric/`-_.` so it can't escape the
    // uploads dir or overwrite system files. A leading dot is allowed so
    // dotfiles (e.g. `.env`) keep their name.
    let safe_name = {
        let raw = filename.trim();
        let stem = raw
            .rsplit(|c| c == '/' || c == '\\')
            .next()
            .unwrap_or("");
        let cleaned: String = stem
            .chars()
            .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
            .collect();
        if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
            "upload.bin".to_string()
        } else {
            cleaned
        }
    };

    // Monotonic suffix avoids collisions when the same filename is uploaded
    // twice — we never want to clobber a previously-staged attachment.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("System clock error: {e}"))?
        .as_millis();
    let path = dir.join(format!("{now}-{safe_name}"));
    fs::write(&path, &bytes)
        .map_err(|e| format!("Failed to write uploaded file: {e}"))?;
    Ok(path.to_string_lossy().to_string())
}

/// Load `config.json`, or `None` when the file does not exist yet (first run).
#[tauri::command]
async fn load_app_config(app: AppHandle) -> Result<Option<AppConfig>, String> {
    let file = get_data_dir(&app)?.join("config.json");
    if !file.exists() {
        return Ok(None);
    }
    let content =
        fs::read_to_string(&file).map_err(|e| format!("Failed to read config.json: {e}"))?;
    // Tolerate a corrupted/partial file by falling back to defaults rather than
    // bricking the app on a bad read.
    let config: AppConfig = serde_json::from_str(&content).unwrap_or_default();
    Ok(Some(config))
}

/// Persist the full app config to `config.json` atomically.
#[tauri::command]
async fn save_app_config(config: AppConfig, app: AppHandle) -> Result<(), String> {
    let data_dir = get_data_dir(&app)?;
    fs::create_dir_all(&data_dir).map_err(|e| format!("Failed to create data directory: {e}"))?;
    let json = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize config: {e}"))?;
    atomic_write(&data_dir.join("config.json"), &json)
}

/// Fire-and-forget: summarize a conversation and write an Obsidian note.
/// Returns immediately after spawning a background task; errors are logged,
/// never surfaced to the caller.
#[tauri::command]
async fn summarize_conversation(
    app: AppHandle,
    conversation_id: String,
    workspace_path: Option<String>,
    title: Option<String>,
    vault_path: Option<String>,
    engine: Option<String>,
    transcript: String,
    force_overwrite: Option<bool>,
) -> Result<(), String> {
    // Resolve vault path to an absolute directory before passing to summarizer.
    let resolved_vault = match vault_path.as_deref() {
        Some(p) if !p.trim().is_empty() => p.to_string(),
        _ => get_data_dir(&app)?.join("kb").to_string_lossy().into_owned(),
    };
    let input = summarizer::SummarizeInput {
        conversation_id,
        vault_path: Some(resolved_vault),
        workspace_path,
        title_hint: title.unwrap_or_default(),
        engine,
        transcript,
        force_overwrite: force_overwrite.unwrap_or(false),
    };
    tokio::spawn(async move {
        if let Err(e) = summarizer::summarize_with_guard(input).await {
            log::error!("[summarize] top-level: {e:#}");
        }
    });
    Ok(())
}

/// Backfill: iterate history, find conversations without existing KB notes,
/// and return the list of (conversation_id, workspace_id, title) that need
/// summarization. The frontend then builds transcripts and calls
/// summarize_conversation for each one.
#[tauri::command]
async fn backfill_list(
    app: AppHandle,
    vault_path: Option<String>,
) -> Result<Vec<BackfillEntry>, String> {
    let data_dir = get_data_dir(&app)?;
    let vault = match vault_path.as_deref() {
        Some(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => data_dir.join("kb"),
    };
    let pixie_dir = vault.join("Pixie");

    // Collect existing conversation IDs from the vault.
    let mut existing_ids = std::collections::HashSet::new();
    if pixie_dir.exists() {
        if let Ok(entries) = fs::read_dir(&pixie_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                // Extract convId from pattern *-<convId>.md
                if let Some(conv_id) = name
                    .strip_suffix(".md")
                    .and_then(|s| s.rsplit_once('-').map(|(_, id)| id.to_string()))
                {
                    existing_ids.insert(conv_id);
                }
            }
        }
    }

    // Load history and find missing entries.
    let history_file = data_dir.join("history.jsonl");
    let mut missing = Vec::new();
    if history_file.exists() {
        let content = fs::read_to_string(&history_file)
            .map_err(|e| format!("Failed to read history: {e}"))?;
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<HistoryEntry>(line) {
                let conv = &entry.conversation;
                let conv_id = conv.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let title = conv.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let _engine = conv
                    .get("engine")
                    .and_then(|v| v.as_str())
                    .unwrap_or("builtin");

                if conv_id.is_empty() || existing_ids.contains(conv_id) {
                    continue;
                }
                // Skip conversations with no messages.
                let msg_count = conv
                    .get("messages")
                    .and_then(|m| m.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                if msg_count == 0 {
                    continue;
                }

                missing.push(BackfillEntry {
                    conversation_id: conv_id.to_string(),
                    workspace_id: entry.workspace_id.clone(),
                    title: title.to_string(),
                });
            }
        }
    }

    Ok(missing)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BackfillEntry {
    conversation_id: String,
    workspace_id: String,
    title: String,
}

/// Search the knowledge base with a BM25 query. Returns ranked results.
#[tauri::command]
async fn search_kb(
    app: AppHandle,
    query: String,
    vault_path: Option<String>,
) -> Result<Vec<search::index::SearchResult>, String> {
    let vault = match vault_path.as_deref() {
        Some(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => get_data_dir(&app)?.join("kb"),
    };
    search::search(&query, &vault, 10)
        .await
        .map_err(|e| e.to_string())
}

/// Return the effective vault path: the configured `vaultPath` if set, otherwise
/// the default `<data_dir>/kb`. Returns `None` if the data dir cannot be resolved.
#[tauri::command]
async fn get_default_vault_path(app: AppHandle) -> Result<Option<String>, String> {
    let kb = get_data_dir(&app)?.join("kb");
    let _ = fs::create_dir_all(&kb);
    Ok(Some(kb.to_string_lossy().into_owned()))
}

/// Initialize a knowledge-base vault directory — ensures `.obsidian/` metadata
/// exists so the folder is recognized as an Obsidian vault.  Call this when the
/// user changes the vault path in Settings, so the vault is ready before they
/// first click "Open in Obsidian" or save a conversation.
#[tauri::command]
async fn initialize_kb_vault(app: AppHandle, vault_path: Option<String>) -> Result<(), String> {
    let path = match vault_path.as_deref() {
        Some(p) if !p.trim().is_empty() => p.to_string(),
        _ => get_data_dir(&app)?.join("kb").to_string_lossy().into_owned(),
    };
    // Create the vault directory if it doesn't exist yet.
    let _ = fs::create_dir_all(&path);
    // Write .obsidian/ metadata.
    ensure_obsidian_vault(&path)?;
    Ok(())
}

/// Ensure the vault directory has `.obsidian/` metadata so Obsidian recognizes
/// it as a valid vault.  Idempotent — safe to call multiple times; creates
/// `app.json` and `appearance.json` only when missing.
pub(crate) fn ensure_obsidian_vault(vault_path: &str) -> Result<(), String> {
    let vault = PathBuf::from(vault_path);
    let obsidian_dir = vault.join(".obsidian");
    fs::create_dir_all(&obsidian_dir).map_err(|e| format!("Cannot create .obsidian: {e}"))?;

    let vault_name = vault
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Pixie".to_string());

    let app_json = obsidian_dir.join("app.json");
    if !app_json.exists() {
        fs::write(&app_json, format!("{{\"vaultName\":\"{vault_name}\"}}"))
            .map_err(|e| format!("Cannot write app.json: {e}"))?;
    }
    let appearance_json = obsidian_dir.join("appearance.json");
    if !appearance_json.exists() {
        fs::write(&appearance_json, "{}").ok();
    }
    Ok(())
}

/// Load every conversation from `history.jsonl` (one entry per line). Malformed
/// lines are skipped so a single bad line can't prevent startup.
#[tauri::command]
async fn load_history(app: AppHandle) -> Result<Vec<HistoryEntry>, String> {
    let file = get_data_dir(&app)?.join("history.jsonl");
    if !file.exists() {
        return Ok(vec![]);
    }
    let content =
        fs::read_to_string(&file).map_err(|e| format!("Failed to read history.jsonl: {e}"))?;
    let mut entries = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<HistoryEntry>(line) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

/// Overwrite `history.jsonl` with one JSON line per entry (full snapshot).
/// The frontend coalesces/debounces calls so this receives the latest state.
#[tauri::command]
async fn save_history(entries: Vec<HistoryEntry>, app: AppHandle) -> Result<(), String> {
    let data_dir = get_data_dir(&app)?;
    fs::create_dir_all(&data_dir).map_err(|e| format!("Failed to create data directory: {e}"))?;
    let mut out = String::new();
    for entry in &entries {
        let line = serde_json::to_string(entry)
            .map_err(|e| format!("Failed to serialize history entry: {e}"))?;
        out.push_str(&line);
        out.push('\n');
    }
    atomic_write(&data_dir.join("history.jsonl"), &out)
}

#[tauri::command]
async fn check_engines_available() -> Result<Vec<EngineStatusResponse>, String> {
    Ok(check_all_engines()
        .await
        .into_iter()
        .map(EngineStatusResponse::from)
        .collect())
}

/// Probe one engine's readiness by sending a tiny "ping" turn and classifying
/// the outcome. Cheap-checks the binary first; if absent, returns immediately
/// with `available=false` (no probe). Otherwise this is a real, billable model
/// call, so callers should cache a `Ready` result rather than re-probing freely.
#[tauri::command]
async fn probe_engine(engine: String) -> Result<EngineStatusResponse, String> {
    Ok(engine::probe_engine(&engine).await.into())
}

#[tauri::command]
async fn list_models(engine: String) -> Result<Vec<serde_json::Value>, String> {
    log::info!("[list_models] called for engine={engine}");
    let models = engine::list_models(&engine).await;
    log::info!(
        "[list_models] engine={engine} returned {} models",
        models.len()
    );
    Ok(models
        .into_iter()
        .map(|(id, label)| serde_json::json!({ "id": id, "label": label }))
        .collect())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn get_data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    // Use Tauri's path resolver so this resolves on every target — including
    // Android, where the `directories` crate has no backend and returns None.
    app.path()
        .app_data_dir()
        .map_err(|e| format!("Failed to determine app data directory: {e}"))
}

/// Log files older than this are purged at startup (see `cleanup_old_logs`).
const LOG_RETENTION: std::time::Duration = std::time::Duration::from_secs(14 * 24 * 60 * 60);

/// Delete rotated/archived log files older than `LOG_RETENTION` so the log
/// directory can't grow without bound over time and across restarts.
///
/// Only files whose name starts with `prefix` and contains ".log" are touched —
/// this catches the active `pixie.log`, dated rotations (`pixie_<date>.log`),
/// and the `.bak` variants the rotator can leave behind. App data files
/// (config.json, history.jsonl, scheduled_tasks.json, …) never match. The active
/// `<prefix>.log` is always preserved regardless of age.
fn cleanup_old_logs(dir: &std::path::Path, prefix: &str, max_age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let active = format!("{}.log", prefix);
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        // Bind the OsString first; borrowing the temporary from file_name()
        // directly would dangle at the end of the let-statement.
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !name.starts_with(prefix) || !name.contains(".log") || name == active {
            continue;
        }
        let Some(age) = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|modified| now.duration_since(modified).ok())
        else {
            continue;
        };
        if age > max_age {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Default knowledge-base vault directory: `<data_dir>/kb`.
/// On Android this resolves to the app's private data directory.
pub(crate) fn default_vault_dir() -> PathBuf {
    PathBuf::from("Pixie")
}

/// Atomically replace `path` with `content`: write to a temp file in the SAME
/// directory, then rename over the target. On macOS `fs::rename` is atomic
/// within one volume (the data dir always is), so a crash mid-write leaves the
/// previous file intact instead of a truncated/corrupt one.
pub(crate) fn atomic_write(path: &std::path::Path, content: &str) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| "target path has no parent".to_string())?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "data".to_string());
    let tmp = dir.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::write(&tmp, content).map_err(|e| format!("Failed to write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("Failed to finalize {}: {e}", path.display()))?;
    Ok(())
}

/// The user-configured default working directory override (Settings), or `None`
/// when unset (fall back to `~/.pixie`). Stored in `default_workspace.txt`.
fn load_default_workspace_override(app: &AppHandle) -> Option<String> {
    let data_dir = get_data_dir(app).ok()?;
    let raw = fs::read_to_string(data_dir.join("default_workspace.txt")).ok()?;
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Persist (`Some`) or clear (`None`) the default working directory override.
fn persist_default_workspace_override(app: &AppHandle, path: &Option<String>) {
    let Ok(data_dir) = get_data_dir(app) else {
        return;
    };
    let _ = fs::create_dir_all(&data_dir);
    let file = data_dir.join("default_workspace.txt");
    match path {
        Some(p) => {
            let _ = fs::write(file, p);
        }
        None => {
            let _ = fs::remove_file(file);
        }
    }
}

// ---------------------------------------------------------------------------
// Scheduled tasks: validation, scheduling, persistence, execution
// ---------------------------------------------------------------------------

/// In-flight task ids, so the same task is never run twice concurrently even if a
/// run outlasts the scheduler tick interval.
static RUNNING_TASKS: std::sync::OnceLock<Mutex<HashSet<String>>> = std::sync::OnceLock::new();
fn running_tasks() -> &'static Mutex<HashSet<String>> {
    RUNNING_TASKS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn validate_schedule(spec: &ScheduleSpec) -> Result<(), String> {
    match spec {
        ScheduleSpec::DailyTime { hour, minute } | ScheduleSpec::WeekdaysTime { hour, minute } => {
            if *hour > 23 {
                return Err("hour must be 0-23".into());
            }
            if *minute > 59 {
                return Err("minute must be 0-59".into());
            }
        }
        ScheduleSpec::EveryNMinutes { minutes } => {
            if *minutes < 1 {
                return Err("minutes must be >= 1".into());
            }
        }
        ScheduleSpec::EveryNHours { hours } => {
            if *hours < 1 {
                return Err("hours must be >= 1".into());
            }
        }
    }
    Ok(())
}

/// Find the next local HH:MM:00 occurrence strictly after `now_local` whose weekday
/// satisfies `day_ok`. Scans today first, then forward. Returns None if no valid
/// candidate exists within a bounded window (defence against hand-edited invalid JSON).
fn next_local_occurrence<F>(
    now_local: DateTime<Local>,
    hour: u32,
    minute: u32,
    day_ok: F,
) -> Option<DateTime<Local>>
where
    F: Fn(u32) -> bool,
{
    let mut date: NaiveDate = now_local.naive_local().date();
    for _ in 0..8 {
        if let Some(naive) = date.and_hms_opt(hour, minute, 0) {
            if let Some(t) = Local.from_local_datetime(&naive).single() {
                if t > now_local && day_ok(t.weekday().num_days_from_monday()) {
                    return Some(t);
                }
            }
        }
        date = date.succ_opt().unwrap_or(date);
    }
    None
}

/// Compute the next fire instant (as a UTC ISO-8601 string) for a schedule, relative to `now_utc`.
/// Schedule fields are interpreted in the user's LOCAL time.
fn compute_next_run(spec: &ScheduleSpec, now_utc: DateTime<Utc>) -> Option<String> {
    let now_local = now_utc.with_timezone(&Local);
    let next_local = match spec {
        ScheduleSpec::DailyTime { hour, minute } => {
            next_local_occurrence(now_local, *hour, *minute, |_| true)?
        }
        ScheduleSpec::WeekdaysTime { hour, minute } => {
            // Monday=1 .. Friday=5 in num_days_from_monday.
            next_local_occurrence(now_local, *hour, *minute, |wd| (1..=5).contains(&wd))?
        }
        ScheduleSpec::EveryNMinutes { minutes } => {
            now_local + chrono::Duration::minutes((*minutes).max(1) as i64)
        }
        ScheduleSpec::EveryNHours { hours } => {
            now_local + chrono::Duration::hours((*hours).max(1) as i64)
        }
    };
    Some(next_local.with_timezone(&Utc).to_rfc3339())
}

fn load_scheduled_tasks(app: &AppHandle) -> Vec<ScheduledTask> {
    let Ok(data_dir) = get_data_dir(app) else {
        return vec![];
    };
    let path = data_dir.join("scheduled_tasks.json");
    if !path.exists() {
        return vec![];
    }
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn persist_scheduled_tasks(app: &AppHandle, tasks: &[ScheduledTask]) -> Result<(), String> {
    let data_dir = get_data_dir(app)?;
    fs::create_dir_all(&data_dir).map_err(|e| format!("Failed to create data dir: {}", e))?;
    let json = serde_json::to_string_pretty(tasks)
        .map_err(|e| format!("Failed to serialize tasks: {}", e))?;
    atomic_write(&data_dir.join("scheduled_tasks.json"), &json)
        .map_err(|e| format!("Failed to write tasks: {}", e))
}

fn load_task_runs(app: &AppHandle) -> Vec<TaskRunRecord> {
    let Ok(data_dir) = get_data_dir(app) else {
        return vec![];
    };
    let path = data_dir.join("task_runs.json");
    if !path.exists() {
        return vec![];
    }
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn record_task_run(app: &AppHandle, record: TaskRunRecord) {
    let Ok(data_dir) = get_data_dir(app) else {
        return;
    };
    let _ = fs::create_dir_all(&data_dir);
    let path = data_dir.join("task_runs.json");
    let mut runs: Vec<TaskRunRecord> = if path.exists() {
        fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        vec![]
    };
    // Dedupe by id (replace) then trim to the most recent 500.
    runs.retain(|r| r.id != record.id);
    runs.push(record);
    let keep_from = runs.len().saturating_sub(500);
    runs.drain(0..keep_from);
    if let Ok(json) = serde_json::to_string_pretty(&runs) {
        let _ = atomic_write(&path, &json);
    }
}

/// Trailing path segment, for short notification bodies.
fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.to_string())
}

/// Run a task's prompt against its workspace headlessly: fire a notification on start,
/// drive a standalone builtin agent session (isolated from interactive chat state), read the
/// full result, record it, fire a completion notification, and emit a `task-run-complete`
/// event so an open window can refresh. Does NOT advance the schedule (the scheduler loop
/// does that before spawning, and manual run-now must not advance).
async fn run_builtin_task_headless(
    app: AppHandle,
    task: ScheduledTask,
    conversation_id: String,
    started: DateTime<Utc>,
    title: String,
) {
    use tauri_plugin_notification::NotificationExt;

    let api_key = engine::builtin::get_api_key();
    if api_key.is_empty() {
        let error = "No ANTHROPIC_API_KEY configured for builtin engine".to_string();
        let _ = app
            .notification()
            .builder()
            .title(&title)
            .body(&error)
            .show();
        record_task_run(
            &app,
            TaskRunRecord {
                id: conversation_id.clone(),
                task_id: task.id.clone(),
                task_name: task.name.clone(),
                workspace: task.workspace.clone(),
                prompt: task.prompt.clone(),
                result: String::new(),
                status: "error".into(),
                started_at: started.to_rfc3339(),
                finished_at: Utc::now().to_rfc3339(),
            },
        );
        let _ = app.emit(
            "task-run-complete",
            serde_json::json!({ "task_id": task.id, "conversation_id": conversation_id, "status": "error" }),
        );
        return;
    }

    let base_url = engine::builtin::get_base_url();
    let mut session =
        BuiltinSession::new(None, None, &task.workspace, &api_key, base_url.as_deref());
    let result = session.run_turn(&task.prompt, &[], |_event| {}).await;
    let finished = Utc::now();

    match result {
        Ok((full_text, had_error)) if !had_error => {
            let preview = if full_text.is_empty() {
                "Completed (no output).".to_string()
            } else {
                full_text.chars().take(160).collect::<String>()
            };
            let _ = app
                .notification()
                .builder()
                .title(&title)
                .body(preview)
                .show();
            record_task_run(
                &app,
                TaskRunRecord {
                    id: conversation_id.clone(),
                    task_id: task.id.clone(),
                    task_name: task.name.clone(),
                    workspace: task.workspace.clone(),
                    prompt: task.prompt.clone(),
                    result: full_text,
                    status: "ok".into(),
                    started_at: started.to_rfc3339(),
                    finished_at: finished.to_rfc3339(),
                },
            );
            let _ = app.emit(
                "task-run-complete",
                serde_json::json!({ "task_id": task.id, "conversation_id": conversation_id, "status": "ok" }),
            );
        }
        Ok((full_text, _)) => {
            let _ = app
                .notification()
                .builder()
                .title(&title)
                .body("Error: builtin engine returned an error event")
                .show();
            record_task_run(
                &app,
                TaskRunRecord {
                    id: conversation_id.clone(),
                    task_id: task.id.clone(),
                    task_name: task.name.clone(),
                    workspace: task.workspace.clone(),
                    prompt: task.prompt.clone(),
                    result: full_text,
                    status: "error".into(),
                    started_at: started.to_rfc3339(),
                    finished_at: finished.to_rfc3339(),
                },
            );
            let _ = app.emit(
                "task-run-complete",
                serde_json::json!({ "task_id": task.id, "conversation_id": conversation_id, "status": "error" }),
            );
        }
        Err(e) => {
            let message = e.to_string();
            log::error!("[scheduled] builtin error for '{}': {}", task.name, message);
            let _ = app
                .notification()
                .builder()
                .title(&title)
                .body(format!("Error: {}", message))
                .show();
            record_task_run(
                &app,
                TaskRunRecord {
                    id: conversation_id.clone(),
                    task_id: task.id.clone(),
                    task_name: task.name.clone(),
                    workspace: task.workspace.clone(),
                    prompt: task.prompt.clone(),
                    result: String::new(),
                    status: "error".into(),
                    started_at: started.to_rfc3339(),
                    finished_at: finished.to_rfc3339(),
                },
            );
            let _ = app.emit(
                "task-run-complete",
                serde_json::json!({ "task_id": task.id, "conversation_id": conversation_id, "status": "error" }),
            );
        }
    }
}

async fn run_task_headless(app: AppHandle, task: ScheduledTask, conversation_id: String) {
    use tauri_plugin_notification::NotificationExt;

    let started = Utc::now();
    let title = format!("⚡ {}", task.name);
    let dir_label = basename(&task.workspace);

    // Notify: started.
    let _ = app
        .notification()
        .builder()
        .title(&title)
        .body(format!("Running in {}…", dir_label))
        .show();

    // Guard against a vanished workspace before spawning.
    if !std::path::Path::new(&task.workspace).is_dir() {
        let _ = app
            .notification()
            .builder()
            .title(&title)
            .body("Workspace no longer exists — skipped.")
            .show();
        record_task_run(
            &app,
            TaskRunRecord {
                id: conversation_id.clone(),
                task_id: task.id.clone(),
                task_name: task.name.clone(),
                workspace: task.workspace.clone(),
                prompt: task.prompt.clone(),
                result: String::new(),
                status: "error".into(),
                started_at: started.to_rfc3339(),
                finished_at: Utc::now().to_rfc3339(),
            },
        );
        let _ = app.emit(
            "task-run-complete",
            serde_json::json!({ "task_id": task.id, "conversation_id": conversation_id, "status": "error" }),
        );
        return;
    }

    if task.engine == "builtin" {
        run_builtin_task_headless(app, task, conversation_id, started, title).await;
    }

    // Only the builtin engine is supported on Android; the external CLI
    // spawn_headless path was removed.
}

/// One scheduler tick: find enabled tasks whose next_run is due and fire them.
/// Skips catch-up (a task stale by more than 5 minutes only advances, never fires)
/// and guards against overlapping runs of the same task.
async fn check_and_run_due_tasks(app: &AppHandle) {
    let now = Utc::now();
    let mut tasks = load_scheduled_tasks(app);
    let mut changed = false;

    for task in tasks.iter_mut() {
        if !task.enabled {
            continue;
        }

        // Resolve the pending fire instant, computing it lazily if missing.
        let next: Option<DateTime<Utc>> = task
            .next_run
            .as_ref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc))
            .or_else(|| {
                compute_next_run(&task.schedule, now)
                    .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                    .map(|d| d.with_timezone(&Utc))
            });

        let Some(next) = next else {
            continue;
        };

        if next > now {
            // Not due yet — keep next_run populated for the panel display.
            if task.next_run.is_none() {
                task.next_run = Some(next.to_rfc3339());
                changed = true;
            }
            continue;
        }

        // Due. If it's stale by more than 5 minutes the app was closed across the
        // scheduled time: advance without firing (skip catch-up).
        let stale = now.signed_duration_since(next);
        if stale.num_minutes() > 5 {
            task.next_run = compute_next_run(&task.schedule, now);
            changed = true;
            continue;
        }

        // Re-entrancy guard: skip if this task is already running.
        {
            let mut running = running_tasks().lock().await;
            if running.contains(&task.id) {
                continue;
            }
            running.insert(task.id.clone());
        }

        // Advance next_run (and mark last_run) BEFORE spawning, persisted before the
        // async run, so a crash or fast re-tick never double-fires.
        task.last_run = Some(now.to_rfc3339());
        task.next_run = compute_next_run(&task.schedule, now);
        changed = true;

        let task_id = task.id.clone();
        let to_run = task.clone();
        let conversation_id = uuid::Uuid::new_v4().to_string();
        let app_for_run = app.clone();
        tauri::async_runtime::spawn(async move {
            run_task_headless(app_for_run, to_run, conversation_id).await;
            running_tasks().lock().await.remove(&task_id);
        });
    }

    if changed {
        if let Err(e) = persist_scheduled_tasks(app, &tasks) {
            log::error!("[scheduler] persist failed: {}", e);
        }
    }
}

#[tauri::command]
async fn list_scheduled_tasks(app: AppHandle) -> Result<Vec<ScheduledTask>, String> {
    Ok(load_scheduled_tasks(&app))
}

#[tauri::command]
async fn create_scheduled_task(
    app: AppHandle,
    mut task: ScheduledTask,
) -> Result<ScheduledTask, String> {
    validate_schedule(&task.schedule)?;
    if task.id.is_empty() {
        task.id = uuid::Uuid::new_v4().to_string();
    }
    task.next_run = if task.enabled {
        compute_next_run(&task.schedule, Utc::now())
    } else {
        None
    };
    task.last_run = None;
    let mut tasks = load_scheduled_tasks(&app);
    tasks.push(task.clone());
    persist_scheduled_tasks(&app, &tasks)?;
    Ok(task)
}

#[tauri::command]
async fn update_scheduled_task(app: AppHandle, mut task: ScheduledTask) -> Result<(), String> {
    validate_schedule(&task.schedule)?;
    task.next_run = if task.enabled {
        compute_next_run(&task.schedule, Utc::now())
    } else {
        None
    };
    let mut tasks = load_scheduled_tasks(&app);
    if let Some(existing) = tasks.iter_mut().find(|t| t.id == task.id) {
        // Preserve last_run across edits.
        task.last_run = existing.last_run.clone();
        *existing = task;
    } else {
        return Err("Task not found".into());
    }
    persist_scheduled_tasks(&app, &tasks)
}

#[tauri::command]
async fn delete_scheduled_task(app: AppHandle, task_id: String) -> Result<(), String> {
    let mut tasks = load_scheduled_tasks(&app);
    tasks.retain(|t| t.id != task_id);
    persist_scheduled_tasks(&app, &tasks)
}

#[tauri::command]
async fn toggle_scheduled_task(
    app: AppHandle,
    task_id: String,
    enabled: bool,
) -> Result<(), String> {
    let mut tasks = load_scheduled_tasks(&app);
    if let Some(t) = tasks.iter_mut().find(|t| t.id == task_id) {
        t.enabled = enabled;
        t.next_run = if enabled {
            compute_next_run(&t.schedule, Utc::now())
        } else {
            None
        };
    } else {
        return Err("Task not found".into());
    }
    persist_scheduled_tasks(&app, &tasks)
}

/// Fire a task immediately (manual "Run now"). Does not advance next_run.
#[tauri::command]
async fn run_scheduled_task_now(app: AppHandle, task_id: String) -> Result<String, String> {
    let tasks = load_scheduled_tasks(&app);
    let task = tasks
        .into_iter()
        .find(|t| t.id == task_id)
        .ok_or_else(|| "Task not found".to_string())?;

    {
        let mut running = running_tasks().lock().await;
        if running.contains(&task_id) {
            return Err("Scheduled task is already running".into());
        }
        running.insert(task_id.clone());
    }

    let conversation_id = uuid::Uuid::new_v4().to_string();
    let app_for_run = app.clone();
    let conv_for_ret = conversation_id.clone();
    let task_id_for_run = task_id.clone();
    tauri::async_runtime::spawn(async move {
        run_task_headless(app_for_run, task, conversation_id).await;
        running_tasks().lock().await.remove(&task_id_for_run);
    });
    Ok(conv_for_ret)
}

#[tauri::command]
async fn list_task_runs(app: AppHandle) -> Result<Vec<TaskRunRecord>, String> {
    Ok(load_task_runs(&app))
}

// ---------------------------------------------------------------------------
// Application entry point
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init());

    builder
        .setup(|app| {
            // Logging: always write to a rotating file in the app data dir
            // (alongside config.json / scheduled_tasks.json) so every run — including
            // release and scheduled loops — is observable. Debug builds also
            // mirror to stdout for the `pnpm tauri dev` terminal.
            {
                let mut targets: Vec<tauri_plugin_log::Target> = Vec::new();
                let mut log_path: Option<PathBuf> = None;
                if let Ok(log_dir) = get_data_dir(app.handle()) {
                    // Purge archived log files older than the retention window
                    // before (re)opening the active log.
                    cleanup_old_logs(&log_dir, "pixie", LOG_RETENTION);
                    log_path = Some(log_dir.join("pixie.log"));
                    targets.push(tauri_plugin_log::Target::new(
                        tauri_plugin_log::TargetKind::Folder {
                            path: log_dir,
                            file_name: Some("pixie".to_string()),
                        },
                    ));
                }
                if cfg!(debug_assertions) {
                    targets.push(tauri_plugin_log::Target::new(
                        tauri_plugin_log::TargetKind::Stdout,
                    ));
                }
                if !targets.is_empty() {
                    let _ = app.handle().plugin(
                        tauri_plugin_log::Builder::new()
                            .level(log::LevelFilter::Info)
                            .targets(targets)
                            .max_file_size(5_000_000) // rotate at ~5 MB
                            .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepSome(3))
                            .build(),
                    );
                    if let Some(p) = log_path {
                        log::info!("[startup] logging to {}", p.display());
                    }
                }
            }

            // --- Scheduled tasks background loop ---
            // Tick every 60s and fire any enabled task whose next_run is due.
            let scheduler_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
                // The first tick fires immediately — run once on startup to catch up,
                // then settle into the 60s cadence.
                loop {
                    ticker.tick().await;
                    check_and_run_due_tasks(&scheduler_handle).await;
                }
            });

            Ok(())
        })
        .manage(AppState {
            conversation_engines: init_conversation_engine_map(),
            workspace: Arc::new(Mutex::new(None)),
            builtin_sessions: init_builtin_sessions(),
        })
        .invoke_handler(tauri::generate_handler![
            send_message,
            set_engine_model_config,
            list_directory,
            read_file_content,
            list_skills,
            stop_generation,
            get_default_workspace_path,
            set_default_workspace_path,
            pick_folder,
            select_workspace,
            pick_files,
            save_pasted_image,
            save_uploaded_file,
            set_active_workspace,
            load_app_config,
            save_app_config,
            load_history,
            save_history,
            check_engines_available,
            probe_engine,
            list_models,
            update_conversation_model,
            list_scheduled_tasks,
            create_scheduled_task,
            update_scheduled_task,
            delete_scheduled_task,
            toggle_scheduled_task,
            run_scheduled_task_now,
            list_task_runs,
            summarize_conversation,
            get_default_vault_path,
            initialize_kb_vault,
            search_kb,
            backfill_list,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_name_and_description() {
        let text = "---\nname: my-skill\ndescription: Does a thing.\n---\n# Body\n";
        let (name, desc, body) = parse_frontmatter(text);
        assert_eq!(name.as_deref(), Some("my-skill"));
        assert_eq!(desc.as_deref(), Some("Does a thing."));
        assert!(body.contains("# Body"));
    }

    #[test]
    fn frontmatter_absent_returns_whole_body() {
        // Mirrors the project's workflows/*.md (no frontmatter).
        let text = "# AI Chat Desktop App\n\nSome content";
        let (name, desc, body) = parse_frontmatter(text);
        assert!(name.is_none());
        assert!(desc.is_none());
        assert!(body.starts_with("# AI Chat Desktop App"));
    }

    #[test]
    fn every_skill_has_name_and_invocation() {
        // Integration check against the real host. Passes on any machine
        // (just asserts invariants on whatever was found); reports the count.
        let skills = list_skills(None).unwrap_or_default();
        let plugin_count = skills.iter().filter(|s| s.source == "plugin").count();
        eprintln!(
            "list_skills(None) -> {} skills ({} plugin)",
            skills.len(),
            plugin_count
        );
        for s in &skills {
            assert!(!s.name.is_empty(), "empty name: {:?}", s);
            assert!(
                s.invocation.starts_with('/') && s.invocation.ends_with(' '),
                "bad invocation: {:?}",
                s
            );
        }
    }

    #[test]
    fn cleanup_old_logs_purges_old_keeps_recent_and_data() {
        use std::time::{Duration, SystemTime};
        let dir = std::env::temp_dir().join(format!("pixie-log-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        // Write a file and backdate its modification time.
        let touch_old = |name: &str, age_secs: u64| {
            let path = dir.join(name);
            std::fs::write(&path, "x").unwrap();
            let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            f.set_modified(SystemTime::now() - Duration::from_secs(age_secs))
                .unwrap();
        };

        touch_old("pixie.log", 10); // active log → always kept
        touch_old("pixie_2025-01-01.log", 30 * 86400); // old rotation → purged
        touch_old("pixie_2025-01-01.log.bak", 30 * 86400); // old .bak → purged
        touch_old("pixie_2099-01-01.log", 1); // recent rotation → kept
        touch_old("config.json", 30 * 86400); // old data file → untouched

        cleanup_old_logs(&dir, "pixie", LOG_RETENTION);

        assert!(
            dir.join("pixie.log").exists(),
            "active log must be preserved"
        );
        assert!(
            !dir.join("pixie_2025-01-01.log").exists(),
            "old rotated log must be purged"
        );
        assert!(
            !dir.join("pixie_2025-01-01.log.bak").exists(),
            "old .bak log must be purged"
        );
        assert!(
            dir.join("pixie_2099-01-01.log").exists(),
            "recent rotated log must be kept"
        );
        assert!(
            dir.join("config.json").exists(),
            "non-log data file must never be touched"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
