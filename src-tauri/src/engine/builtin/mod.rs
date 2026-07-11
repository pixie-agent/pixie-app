//! Builtin engine — an in-process agent loop powered by the `pixie-pi` library.
//!
//! The other engines (claude, cursor, codebuddy) spawn an external CLI process
//! and parse its NDJSON. The builtin engine instead runs the agent loop directly
//! in Rust, driving [`pixie_pi::AgentSession`] and mapping its [`AgentEvent`]s
//! onto pixie's engine-agnostic [`NormalizedEvent`]s over an mpsc channel.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use futures_util::StreamExt;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use pixie_pi::ai::stream::AssistantMessageEvent;
use pixie_pi::ai::types::{Api, ToolResultContent};
use pixie_pi::{AgentEvent, AgentSession, Message, Model, ThinkingLevel, UserMessage};

use super::{EngineStatus, NormalizedEvent, ToolEvent, ToolEventKind, UsageInfo};

/// Fallback Anthropic model for the builtin engine when no model is configured.
pub const DEFAULT_MODEL: &str = "claude-sonnet-4-20250514";

// ---------------------------------------------------------------------------
// BuiltinSession — wraps a pixie_pi::AgentSession
// ---------------------------------------------------------------------------

pub struct BuiltinSession {
    session: AgentSession,
    cancel_token: CancellationToken,
}

impl BuiltinSession {
    /// Create a new session. Signature matches the old hand-rolled builtin so
    /// `lib.rs` is unchanged.
    pub fn new(
        model: Option<&str>,
        system_prompt: Option<&str>,
        cwd: &str,
        api_key: &str,
        base_url: Option<&str>,
    ) -> Self {
        // A caller-provided model wins; otherwise fall back to the configured
        // (ANTHROPIC_MODEL) or default model.
        let model_pattern = model.map(str::to_string).unwrap_or_else(get_model);
        let resolved = resolve_builtin_model(Some(&model_pattern), base_url);
        log::info!(
            "[builtin] new session: model={}, base_url={}, cwd={}",
            resolved.id,
            resolved.base_url,
            cwd
        );

        // No bash/shell tool: it can't run in the Android sandbox. The agent can
        // still read/write/edit/grep/find/ls files rooted at the workspace cwd.
        let tools = pixie_pi::tools::select_tools(
            PathBuf::from(cwd),
            &[
                "read".to_string(),
                "edit".to_string(),
                "write".to_string(),
                "grep".to_string(),
                "find".to_string(),
                "ls".to_string(),
            ],
        );
        let system = system_prompt
            .unwrap_or(
                "You are a helpful coding assistant. Use the provided tools to read, \
                 edit, write, and search files in the workspace as needed.",
            )
            .to_string();

        let mut session = AgentSession::new(
            PathBuf::from(cwd),
            system,
            resolved,
            ThinkingLevel::Off,
            tools,
            reqwest::Client::new(),
        );
        session.api_key = if api_key.is_empty() {
            None
        } else {
            Some(api_key.to_string())
        };

        Self {
            session,
            cancel_token: CancellationToken::new(),
        }
    }

    /// Run a single turn: feed the user message to the agent loop and stream its
    /// events to the frontend as `NormalizedEvent`s. Returns the turn's final text.
    pub async fn run_turn(
        &mut self,
        message: &str,
        // TODO: forward image blocks to pixie_pi::UserMessage once its API supports them.
        _images: &[String],
        emit: impl FnMut(NormalizedEvent),
    ) -> Result<(String, bool)> {
        self.run_turn_with_cancel_token(message, _images, CancellationToken::new(), emit)
            .await
    }

    /// Run a turn using a caller-owned cancellation token. Background loops use
    /// this so stop/discard can cancel an in-process builtin turn immediately.
    pub async fn run_turn_with_cancel_token(
        &mut self,
        message: &str,
        // TODO: forward image blocks to pixie_pi::UserMessage once its API supports them.
        _images: &[String],
        cancel_token: CancellationToken,
        mut emit: impl FnMut(NormalizedEvent),
    ) -> Result<(String, bool)> {
        self.cancel_token = cancel_token;

        let model_id = self.session.model.id.clone();
        let prompts = vec![Message::User(UserMessage::text(message))];

        // `run` clones the live transcript into the loop; the loop appends to its
        // copy and ends with `AgentEnd { messages }`. We capture that final
        // transcript and write it back so the next turn has the full history.
        let mut stream = self.session.run(prompts, self.cancel_token.clone());

        let mut final_text = String::new();
        let mut had_error = false;
        let mut final_messages: Option<Vec<Message>> = None;

        // Emit each event IMMEDIATELY via the caller's closure so deltas reach
        // the frontend in real time — NOT buffered in a channel until the turn
        // ends (which caused the long "no response" gap before text appeared).
        while let Some(ev) = stream.next().await {
            let ev_name = match &ev {
                AgentEvent::AgentStart => "AgentStart",
                AgentEvent::AgentEnd { .. } => "AgentEnd",
                AgentEvent::TurnStart => "TurnStart",
                AgentEvent::TurnEnd { .. } => "TurnEnd",
                AgentEvent::MessageStart(_) => "MessageStart",
                AgentEvent::MessageUpdate { event } => match event {
                    AssistantMessageEvent::TextDelta { delta, .. } => {
                        log::info!("[builtin] TextDelta len={}", delta.len());
                        "MessageUpdate/TextDelta"
                    }
                    AssistantMessageEvent::ThinkingDelta { .. } => "MessageUpdate/ThinkingDelta",
                    AssistantMessageEvent::Done { message, .. } => {
                        log::info!(
                            "[builtin] Done event, text_len={}",
                            message.text_content().len()
                        );
                        "MessageUpdate/Done"
                    }
                    AssistantMessageEvent::Error { message, .. } => {
                        log::info!(
                            "[builtin] Error event, text_len={}",
                            message.text_content().len()
                        );
                        "MessageUpdate/Error"
                    }
                    _ => "MessageUpdate/other",
                },
                AgentEvent::MessageEnd(msg) => {
                    let text_len = match msg {
                        Message::Assistant(a) => a.text_content().len(),
                        _ => 0,
                    };
                    log::info!("[builtin] MessageEnd text_len={}", text_len);
                    "MessageEnd"
                }
                AgentEvent::ToolExecutionStart { .. } => "ToolExecutionStart",
                AgentEvent::ToolExecutionEnd { .. } => "ToolExecutionEnd",
                AgentEvent::Usage(_) => "Usage",
                AgentEvent::Error(_) => "Error",
            };
            log::info!("[builtin] stream event: {}", ev_name);
            map_agent_event(&ev, &model_id, &mut final_text, &mut had_error, &mut emit);
            if let AgentEvent::AgentEnd { messages } = &ev {
                final_messages = Some(messages.clone());
            }
        }

        // Write the authoritative transcript back for multi-turn continuity.
        if let Some(msgs) = final_messages {
            self.session.messages = msgs;
        }

        log::info!(
            "[builtin] turn finished: final_text_len={}, had_error={}",
            final_text.len(),
            had_error
        );

        if !had_error {
            emit(NormalizedEvent::Final {
                text: final_text.clone(),
            });
        }

        Ok((final_text, had_error))
    }

    /// Cancel the current turn.
    pub fn cancel(&self) {
        self.cancel_token.cancel();
    }
}

// ---------------------------------------------------------------------------
// Session map (shared state)
// ---------------------------------------------------------------------------

pub type BuiltinSessionMap = Arc<Mutex<HashMap<String, BuiltinSession>>>;

pub fn init_builtin_sessions() -> BuiltinSessionMap {
    Arc::new(Mutex::new(HashMap::new()))
}

// ---------------------------------------------------------------------------
// Engine check / probe / models
// ---------------------------------------------------------------------------

/// "Available" means an API key is configured — there is no external binary.
pub async fn check_available() -> EngineStatus {
    let api_key = get_api_key();
    let available = !api_key.is_empty();
    EngineStatus::basic(
        "builtin",
        engine_display_name(),
        available,
        Some("builtin".to_string()),
        None,
        if available {
            None
        } else {
            Some("No ANTHROPIC_API_KEY configured".to_string())
        },
    )
}

/// List available models. For the builtin engine, we expose a single
/// "default" model option. The actual model ID is configured via
/// ANTHROPIC_MODEL environment variable or config file.
pub async fn list_models() -> Vec<(String, String)> {
    vec![(
        "builtin".to_string(),
        "Default (configured via ANTHROPIC_MODEL)".to_string(),
    )]
}

pub fn engine_display_name() -> &'static str {
    "Builtin"
}

// ---------------------------------------------------------------------------
// AgentEvent → NormalizedEvent mapping
// ---------------------------------------------------------------------------

/// Map one pixie-pi [`AgentEvent`] to zero or more [`NormalizedEvent`]s, sending
/// them on `tx`. Updates `final_text` / `had_error` for the caller. Returns
/// `true` when the event is the turn terminator (`AgentEnd`).
fn map_agent_event(
    ev: &AgentEvent,
    model_id: &str,
    final_text: &mut String,
    had_error: &mut bool,
    emit: &mut impl FnMut(NormalizedEvent),
) {
    match ev {
        // Live text/thinking deltas → stream straight through AND accumulate
        // into final_text so the command return value contains the full
        // response even when MessageEnd.text_content() is empty.
        AgentEvent::MessageUpdate { event } => match event {
            AssistantMessageEvent::TextDelta { delta, .. } => {
                final_text.push_str(delta);
                emit(NormalizedEvent::TextDelta {
                    text: delta.clone(),
                    event_type: "delta",
                });
            }
            AssistantMessageEvent::ThinkingDelta { .. } => {
                // Thinking deltas not accumulated into final_text
            }
            // The "Done" / "Error" stop events carry the full assembled
            // AssistantMessage — extract text here as a fallback when the
            // API doesn't stream TextDelta events (e.g. glm via Anthropic-
            // compatible endpoint).
            AssistantMessageEvent::Done { message, .. }
            | AssistantMessageEvent::Error { message, .. } => {
                let text = message.text_content();
                if !text.is_empty() && final_text.is_empty() {
                    *final_text = text.clone();
                }
                if !text.is_empty() {
                    emit(NormalizedEvent::TextDelta {
                        text,
                        event_type: "final",
                    });
                }
            }
            _ => {}
        },

        // Tool call starts (the result content arrives later in TurnEnd).
        AgentEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
        } => {
            emit(NormalizedEvent::Tool(ToolEvent {
                id: tool_call_id.clone(),
                kind: ToolEventKind::Start {
                    name: Some(tool_name.clone()),
                    input: Some(args.to_string()),
                },
            }));
        }

        // End of an assistant turn: emit a Result event per tool call, carrying
        // its content. (ToolExecutionEnd only has is_error, no content.)
        AgentEvent::TurnEnd { tool_results, .. } => {
            for r in tool_results {
                let text = r
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        ToolResultContent::Text { text } => Some(text.as_str()),
                        ToolResultContent::Image { .. } => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                emit(NormalizedEvent::Tool(ToolEvent {
                    id: r.tool_call_id.clone(),
                    kind: ToolEventKind::Result {
                        content: Some(text),
                        is_error: r.is_error,
                    },
                }));
            }
        }

        AgentEvent::Usage(u) => {
            emit(NormalizedEvent::Usage(UsageInfo {
                kind: "turn",
                input_tokens: u.input,
                output_tokens: u.output,
                cache_read_tokens: u.cache_read,
                cache_creation_tokens: u.cache_write,
                cost_usd: None,
                duration_ms: None,
                num_turns: None,
                model: Some(model_id.to_string()),
                stop_reason: None,
            }));
        }

        // Remember the latest assistant text as the turn's final answer.
        AgentEvent::MessageEnd(Message::Assistant(a)) => {
            let t = a.text_content();
            if !t.is_empty() {
                *final_text = t;
            }
        }

        AgentEvent::Error(msg) => {
            log::error!("[builtin] agent loop error: {}", msg);
            *had_error = true;
            emit(NormalizedEvent::Error {
                message: msg.clone(),
            });
        }

        // Turn terminator — nothing to emit; the caller writes the carried
        // transcript back from the event itself.
        AgentEvent::AgentEnd { .. } => {}

        // AgentStart / TurnStart / MessageStart / ToolExecutionEnd — nothing to emit.
        _ => {}
    }
}

/// Resolve the model from an optional override pattern + base URL.
/// For the builtin engine, we use the provided model ID directly (no registry lookup).
/// The model ID should already be resolved to a real Anthropic model ID by the caller.
fn resolve_builtin_model(model: Option<&str>, base_url: Option<&str>) -> Model {
    // Use the provided model ID, or get from config/env
    let model_id = if let Some(m) = model {
        // Skip the placeholder "builtin" - it should have been resolved earlier
        if m == "builtin" {
            log::warn!("[builtin] Received 'builtin' placeholder, resolving to actual model");
            get_model()
        } else {
            m.to_string()
        }
    } else {
        get_model()
    };

    log::info!(
        "[builtin] resolving model: id={}, base_url={:?}",
        model_id,
        base_url
    );

    // Resolve base_url with fallback chain
    let base_url = base_url.map(|u| u.to_string()).unwrap_or_else(|| {
        std::env::var("ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| "https://api.anthropic.com".to_string())
    });

    Model {
        id: model_id.to_string(),
        provider: "anthropic".to_string(),
        api: Api::AnthropicMessages,
        max_tokens: 64_000,
        context_window: 200_000,
        base_url,
        reasoning: true, // Enable reasoning for better performance
        force_adaptive_thinking: false,
        supports_temperature: false,
        input_cost_per_mtok: 3.0,
        output_cost_per_mtok: 15.0,
        cache_read_cost_per_mtok: 0.30,
        cache_write_cost_per_mtok: 3.75,
    }
}

// ---------------------------------------------------------------------------
// Config helpers
// ---------------------------------------------------------------------------

/// API key priority: builtin config → ANTHROPIC_API_KEY env.
pub fn get_api_key() -> String {
    use super::shared::get_model_config_value;

    if let Some(v) = get_model_config_value("builtin", "ANTHROPIC_API_KEY") {
        log::info!("[builtin] using API key from builtin config");
        return v;
    }
    if let Ok(v) = std::env::var("ANTHROPIC_API_KEY") {
        if !v.is_empty() {
            log::info!("[builtin] using API key from ANTHROPIC_API_KEY env var");
            return v;
        }
    }
    log::warn!("[builtin] no API key found: checked builtin config and ANTHROPIC_API_KEY env var");
    String::new()
}

/// Base URL priority: builtin config → ANTHROPIC_BASE_URL env.
pub fn get_base_url() -> Option<String> {
    use super::shared::get_model_config_value;
    get_model_config_value("builtin", "ANTHROPIC_BASE_URL").or_else(|| {
        std::env::var("ANTHROPIC_BASE_URL")
            .ok()
            .filter(|v| !v.is_empty())
    })
}

/// Model priority: builtin config → ANTHROPIC_MODEL env → default.
pub fn get_model() -> String {
    use super::shared::get_model_config_value;
    get_model_config_value("builtin", "ANTHROPIC_MODEL")
        .or_else(|| {
            std::env::var("ANTHROPIC_MODEL")
                .ok()
                .filter(|v| !v.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_model_is_a_real_model_id_not_ui_placeholder() {
        assert_ne!(DEFAULT_MODEL, "builtin");
    }
}
