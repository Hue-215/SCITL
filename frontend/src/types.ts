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
  | { type: 'tool_call'; id: string | null; name: string; arguments: unknown }
  | { type: 'done'; finish_reason: 'stop' | 'tool_call' | 'length' | 'error' }

export interface ChatEntry {
  role: 'user' | 'assistant' | 'tool'
  content: string
}

// crates/scitl-tauri/src/commands/settings.rs の型と一致させる。
export type ApiFormat = 'open_ai_compat'

export interface GeneralSettings {
  system_prompt: string | null
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

export interface SettingsView {
  general: GeneralSettings
  providers: ProviderView[]
  active_provider_id: string | null
}
