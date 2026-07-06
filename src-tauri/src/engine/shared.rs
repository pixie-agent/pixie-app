use std::collections::HashMap;
use std::sync::{Mutex as StdMutex, OnceLock};

static MODEL_CONFIGS: OnceLock<StdMutex<HashMap<String, HashMap<String, String>>>> =
    OnceLock::new();

fn get_model_configs() -> &'static StdMutex<HashMap<String, HashMap<String, String>>> {
    MODEL_CONFIGS.get_or_init(|| StdMutex::new(HashMap::new()))
}

pub fn set_engine_model_config(engine: &str, config: HashMap<String, String>) {
    if let Ok(mut guard) = get_model_configs().lock() {
        guard.insert(engine.to_string(), config);
    }
}

/// Get a single config value for an engine by key.
/// Returns None if the key is not set or the value is empty.
pub fn get_model_config_value(engine: &str, key: &str) -> Option<String> {
    if let Ok(guard) = get_model_configs().lock() {
        if let Some(overrides) = guard.get(engine) {
            if let Some(v) = overrides.get(key) {
                if !v.is_empty() {
                    return Some(v.clone());
                }
            }
        }
    }
    None
}

pub const MAX_TOOL_RESULT_CHARS: usize = 8_000;

pub fn truncate_text(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max).collect();
    format!("{truncated}… (truncated)")
}
