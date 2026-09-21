// crates/scitl-core の型とシリアライズ形式を一致させる。
// (docs/spec/rebuild/data-model.md, crates/scitl-core/src/llm/mod.rs)

export interface Task {
  id: number
  title: string | null
  description: string | null
  deadline: string | null
  archived_at: string | null
  deleted_at: string | null
  created_at: string
  updated_at: string
}

export interface TaskSummary {
  id: number
  title: string | null
  deadline: string | null
  archived_at: string | null
  steps_done: number
  steps_total: number
}

export type ResponseEvent =
  | { type: 'text_delta'; text: string }
  // モデルの思考(reasoning)の断片(Issue #42)。表示・保存専用で、APIへの再送信には
  // 使わない(docs/spec/principles.md 3節)。
  | { type: 'reasoning_delta'; text: string }
  | { type: 'tool_call'; id: string | null; name: string; arguments: unknown }
  | { type: 'done'; finish_reason: 'stop' | 'tool_call' | 'length' | 'error' }

// crates/scitl-core/src/db/messages.rs の Message と一致させる。
export interface Message {
  id: number
  task_id: number | null
  role: 'user' | 'assistant' | 'tool' | 'error'
  content: string
  kind: 'normal' | 'tool_execution'
  source: string | null
  // 表示・エクスポート専用。APIへは送らない(data-model.md messagesテーブル)。
  reasoning: string | null
  error_kind: string | null
  turn_id: string | null
  attempt_no: number | null
  created_at: string
}

// DBに未確定の、送信直後の楽観表示専用のプレースホルダ(principles.md 3節「保存するのは
// 組み立て終わった応答」に従い、確定後はlist_task_messagesで引き直して置き換える)。
export interface PendingEntry {
  role: 'user' | 'pending'
  content: string
}

// crates/scitl-tauri/src/commands/settings.rs の型と一致させる。
export type ApiFormat = 'open_ai_compat'

export interface GeneralSettings {
  system_prompt: string | null
  task_chat_system_prompt: string | null
  response_timeout_secs: number | null
}

export interface ProviderView {
  id: string
  name: string
  api_format: ApiFormat
  base_url: string
  models: string[]
  active_model: string | null
  has_api_key: boolean
}

export type McpEndpointView =
  | { transport: 'stdio'; command: string; args: string[]; env_names: string[] }
  | { transport: 'streamable_http'; url: string; header_names: string[] }

export interface McpServerView {
  id: string
  name: string
  enabled: boolean
  endpoint: McpEndpointView
  enabled_tools: string[]
  // 取得済みのツール一覧(Issue #104)。nullは「まだ取得していない」。
  // 取得結果はRust側のキャッシュが持ち、画面はそれを描くだけ。
  tools: McpToolInfo[] | null
}

export interface McpToolInfo {
  name: string
  description: string | null
}

export interface SettingsView {
  general: GeneralSettings
  providers: ProviderView[]
  active_provider_id: string | null
  mcp_servers: McpServerView[]
}
