pub mod builtin;
pub(crate) mod shared;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

pub use shared::{set_engine_model_config, truncate_text, MAX_TOOL_RESULT_CHARS};

/// Registered agent engine identifiers.
///
/// There is exactly one engine: `builtin`, which runs the agent loop in-process
/// (see `engine/builtin`) and spawns no CLI. Adding a subprocess-based engine is
/// not a small plugin add — the old `spawn_*` / `parse_line` / readiness-probe
/// plumbing was removed when the project went builtin-only and would have to be
/// resurrected. See CLAUDE.md ("Engine: builtin, in-process").
pub const ENGINE_IDS: &[&str] = &["builtin"];

pub fn normalize_engine_id(id: &str) -> Result<&'static str> {
    match id {
        "builtin" => Ok("builtin"),
        other => anyhow::bail!("unknown engine: {other}"),
    }
}

pub fn engine_display_name(id: &str) -> &'static str {
    match id {
        "builtin" => builtin::engine_display_name(),
        _ => "Unknown",
    }
}

/// How far an engine has been verified.
///
/// The builtin engine has no binary or credential store, so readiness is a
/// single cheap check: is an API key configured? `check_available` marks it
/// `Ready` when a key is present and leaves it `Unknown` otherwise. The remaining
/// variants (`NotAuthenticated`, `Error`, `NoResponse`) and the old billable
/// "ping" probe existed for the removed subprocess engines and are unused by the
/// builtin path today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuthState {
    #[default]
    Unknown,
    Ready,
    NotAuthenticated,
    Error,
    NoResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineStatus {
    pub id: String,
    pub display_name: String,
    pub available: bool,
    pub version: Option<String>,
    pub path: Option<String>,
    pub error: Option<String>,
    /// Result of the readiness check. `Unknown` until `check_available` runs.
    #[serde(default)]
    pub auth_state: AuthState,
    /// Raw engine message accompanying a non-`Ready` probe outcome.
    #[serde(default)]
    pub probe_error: Option<String>,
}

impl EngineStatus {
    /// Build a status from the cheap binary/version check only (auth not probed).
    /// Centralizes the `auth_state = Unknown` default so engine modules don't
    /// repeat the new fields in every `check_available` arm.
    pub fn basic(
        id: &str,
        display_name: &str,
        available: bool,
        version: Option<String>,
        path: Option<String>,
        error: Option<String>,
    ) -> Self {
        Self {
            id: id.to_string(),
            display_name: display_name.to_string(),
            available,
            version,
            path,
            error,
            auth_state: AuthState::Unknown,
            probe_error: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Normalized stream events (engine-agnostic)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum ToolEventKind {
    Start {
        name: Option<String>,
        input: Option<String>,
    },
    Result {
        content: Option<String>,
        is_error: bool,
    },
}

#[derive(Debug, Clone)]
pub struct ToolEvent {
    pub id: String,
    pub kind: ToolEventKind,
}

#[derive(Debug, Clone)]
pub struct UsageInfo {
    pub kind: &'static str,
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

#[derive(Debug, Clone)]
pub enum NormalizedEvent {
    TextDelta {
        text: String,
        event_type: &'static str,
    },
    ThinkingText {
        content: String,
    },
    Tool(ToolEvent),
    Usage(UsageInfo),
    Final {
        text: String,
    },
    Error {
        message: String,
    },
}

impl NormalizedEvent {
    pub fn streaming_text(&self) -> Option<String> {
        match self {
            NormalizedEvent::TextDelta { text, .. } => Some(text.clone()),
            _ => None,
        }
    }

    pub fn streaming_text_event_type(&self) -> Option<&'static str> {
        match self {
            NormalizedEvent::TextDelta { event_type, .. } => Some(event_type),
            _ => None,
        }
    }

    pub fn streaming_thinking(&self) -> Option<String> {
        match self {
            NormalizedEvent::ThinkingText { content } => Some(content.clone()),
            _ => None,
        }
    }

    pub fn tool_event(&self) -> Option<&ToolEvent> {
        match self {
            NormalizedEvent::Tool(te) => Some(te),
            _ => None,
        }
    }

    pub fn usage(&self) -> Option<&UsageInfo> {
        match self {
            NormalizedEvent::Usage(u) => Some(u),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub fn final_text(&self) -> Option<String> {
        match self {
            NormalizedEvent::Final { text } => Some(text.clone()),
            NormalizedEvent::Error { message } => Some(message.clone()),
            _ => None,
        }
    }
}

pub async fn check_engine(id: &str) -> EngineStatus {
    let display_name = engine_display_name(id);
    match id {
        "builtin" => builtin::check_available().await,
        _ => EngineStatus::basic(
            id,
            display_name,
            false,
            None,
            None,
            Some(format!("unknown engine: {id}")),
        ),
    }
}

pub async fn check_all_engines() -> Vec<EngineStatus> {
    let mut out = Vec::with_capacity(ENGINE_IDS.len());
    for id in ENGINE_IDS {
        out.push(check_engine(id).await);
    }
    out
}

// ---------------------------------------------------------------------------
// Readiness / auth probe
//
// The builtin engine has no credential store to inspect — it reads an API key
// from config/env — so readiness is a single cheap check: is an API key
// configured? We deliberately do NOT send a billable "ping" turn; `available`
// just means a key is present, and we mark the engine Ready on that basis.
// ---------------------------------------------------------------------------

/// Probe a single engine's readiness. For the builtin engine this is the same
/// cheap API-key check as [`check_engine`] — no network call is made.
pub async fn probe_engine(id: &str) -> EngineStatus {
    let status = check_engine(id).await;
    if !status.available {
        // Binary missing — nothing to probe; auth_state stays Unknown.
        return status;
    }
    builtin_probe(id).await
}

/// Probe the builtin engine by making a minimal API call to Anthropic.
async fn builtin_probe(id: &str) -> EngineStatus {
    let mut status = check_engine(id).await;
    if !status.available {
        return status;
    }
    status.auth_state = AuthState::Ready;
    status
}

/// Fetch available models for a given engine.
/// Returns a list of (model_id, display_label) pairs.
pub async fn list_models(engine_id: &str) -> Vec<(String, String)> {
    match engine_id {
        "builtin" => builtin::list_models().await,
        _ => vec![],
    }
}

/// Per-conversation engine binding + optional model override.
#[derive(Debug, Clone)]
pub struct ConversationEngineState {
    pub engine_id: String,
    /// Per-conversation model override (empty = use engine's global config).
    pub model: Option<String>,
}

pub type ConversationEngineMap = Arc<Mutex<HashMap<String, ConversationEngineState>>>;

pub fn init_conversation_engine_map() -> ConversationEngineMap {
    Arc::new(Mutex::new(HashMap::new()))
}

pub async fn bind_conversation_engine(
    map: &ConversationEngineMap,
    conversation_id: &str,
    engine_id: &str,
) {
    let mut guard = map.lock().await;
    guard
        .entry(conversation_id.to_string())
        .or_insert_with(|| ConversationEngineState {
            engine_id: engine_id.to_string(),
            model: None,
        })
        .engine_id = engine_id.to_string();
}

pub async fn conversation_engine_id(
    map: &ConversationEngineMap,
    conversation_id: &str,
) -> Option<String> {
    let guard = map.lock().await;
    guard.get(conversation_id).map(|s| s.engine_id.clone())
}

pub async fn set_conversation_model(
    map: &ConversationEngineMap,
    conversation_id: &str,
    model: Option<String>,
) {
    let mut guard = map.lock().await;
    if let Some(entry) = guard.get_mut(conversation_id) {
        entry.model = model;
    }
}
