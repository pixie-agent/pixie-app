export interface ToolStep {
  id: string;
  name: string;
  status: "running" | "done" | "error";
  input?: unknown;
  rawInput?: string;
  result?: string;
}

export interface MessageUsage {
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
  costUsd?: number;
  durationMs?: number;
  numTurns?: number;
  model?: string;
  stopReason?: string;
  /** true while still streaming (running totals), false once authoritative final arrives */
  live?: boolean;
}

export interface Message {
  id: string;
  role: "user" | "assistant" | "system";
  content: string;
  /** Absolute paths of image attachments sent with this user message. Rendered
   *  as thumbnails in the bubble; sent to the engine as image content blocks. */
  images?: string[];
  timestamp: number;
  status?: "sending" | "streaming" | "done" | "error";
  tools?: ToolStep[];
  usage?: MessageUsage;
  thinkingTokens?: number;
  thinking?: string;
}

export interface Conversation {
  id: string;
  title: string;
  messages: Message[];
  createdAt: number;
  updatedAt: number;
  /** Agent engine bound to this session. */
  engine: AgentEngineId;
  /** Per-conversation model override. When empty/undefined, uses the engine's global config. */
  model?: string;
}

export type AgentEngineId = "builtin";

/** Env key that each engine uses for its model override in global config. */
export const ENGINE_MODEL_ENV_KEY: Record<AgentEngineId, string> = {
  builtin: "ANTHROPIC_MODEL",
};

/** A model entry returned by the backend's list_models command. */
export interface ModelEntry {
  id: string;
  label: string;
}

export const AGENT_ENGINES: { id: AgentEngineId; label: string }[] = [
  { id: "builtin", label: "Builtin" },
];

/** Readiness/auth state of an engine, set by the backend probe. Mirrors the
 *  Rust `AuthState` enum (serde `snake_case`). `unknown` until a probe runs. */
export type AuthState =
  | "unknown"
  | "ready"
  | "not_authenticated"
  | "error"
  | "no_response";

export interface EngineStatus {
  id: AgentEngineId;
  display_name: string;
  available: boolean;
  version?: string;
  path?: string;
  error?: string;
  /** Result of the readiness ping probe. Absent on older cached values. */
  auth_state?: AuthState;
  /** Raw engine message for a non-`ready` probe outcome. */
  probe_error?: string | null;
}

export interface ResponseChunk {
  conversation_id: string;
  content: string;
  event_type: string;
}

export interface ResponseDone {
  conversation_id: string;
  full_text: string;
}

export interface ResponseTool {
  conversation_id: string;
  tool_use_id: string;
  kind: "start" | "result";
  name?: string;
  input?: string;
  content?: string;
  is_error: boolean;
}

export interface ResponseUsage {
  conversation_id: string;
  kind: "turn" | "final";
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_creation_tokens: number;
  cost_usd?: number;
  duration_ms?: number;
  num_turns?: number;
  model?: string;
  stop_reason?: string;
}

export interface ResponseThinking {
  conversation_id: string;
  tokens: number;
}

export interface ResponseThinkingText {
  conversation_id: string;
  content: string;
}

export interface ResponseError {
  conversation_id: string;
  error: string;
}

export interface WorkspaceState {
  id: string;
  path: string;
  name: string;
}

export interface BuiltinModelConfig {
  /** Anthropic API key */
  ANTHROPIC_API_KEY?: string;
  /** Custom Anthropic API base URL */
  ANTHROPIC_BASE_URL?: string;
  /** Model override for builtin engine */
  ANTHROPIC_MODEL?: string;
}

/** Per-engine model/env overrides. */
export type EngineModelConfigs = {
  builtin: BuiltinModelConfig;
};

export const DEFAULT_ENGINE_MODEL_CONFIGS: EngineModelConfigs = {
  builtin: {},
};

export const ENGINE_MODEL_FIELDS: Record<
  AgentEngineId,
  { key: string; label: string; secret?: boolean }[]
> = {
  builtin: [
    { key: "ANTHROPIC_API_KEY", label: "API Key", secret: true },
    { key: "ANTHROPIC_BASE_URL", label: "Base URL" },
    { key: "ANTHROPIC_MODEL", label: "Model" },
  ],
};

export interface FileEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
}

/** An agent skill discovered on disk (user-, project- or plugin-level). Uses the Claude agent skills standard (SKILL.md + frontmatter). */
export interface SkillEntry {
  name: string;
  description: string;
  source: "user" | "project" | "plugin";
  /** Text inserted into the input when picked, e.g. "/skill-name ". */
  invocation: string;
}

/** A preview-open request (what callers pass to the handler). */
export type PreviewRequest =
  | { kind: "file"; path: string }
  | { kind: "url"; url: string };

/** PreviewRequest plus a nonce so the panel can re-open the same target. */
export type PreviewTarget = PreviewRequest & { nonce: number };

// ---------------------------------------------------------------------------
// Scheduled tasks
// ---------------------------------------------------------------------------

/** A schedule preset. `type` discriminant matches the Rust `#[serde(tag="type")]` enum.
 * Fields are authored in the user's LOCAL time. */
export type ScheduleSpec =
  | { type: "daily_time"; hour: number; minute: number }
  | { type: "every_n_minutes"; minutes: number }
  | { type: "every_n_hours"; hours: number }
  | { type: "weekdays_time"; hour: number; minute: number };

export interface ScheduledTask {
  id: string;
  name: string;
  /** Workspace folder path (= WorkspaceState.id/path). Used as the agent CWD. */
  workspace: string;
  prompt: string;
  schedule: ScheduleSpec;
  enabled: boolean;
  /** Engine to use for this task. Defaults to 'builtin'. */
  engine: AgentEngineId;
  /** ISO-8601 (UTC) of the next pending fire, or null when disabled. */
  next_run: string | null;
  /** ISO-8601 (UTC) of the last fire. */
  last_run: string | null;
}

/** A completed (or failed) scheduled-task execution record. */
export interface TaskRunRecord {
  id: string;
  task_id: string;
  task_name: string;
  workspace: string;
  prompt: string;
  result: string;
  status: "ok" | "error";
  started_at: string;
  finished_at: string;
}

/** BM25 search result from the knowledge base. */
export interface KbSearchResult {
  title: string;
  conversation_id: string;
  path: string;
  snippet: string;
  tags: string[];
  created: string;
  score: number;
}

/** Statistics about the search index. */
export interface SearchIndexStats {
  doc_count: number;
  term_count: number;
}
