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

export type ResponseEvent =
  | { type: 'text_delta'; text: string }
  | { type: 'tool_call'; name: string; arguments: unknown }
  | { type: 'done'; finish_reason: 'stop' | 'tool_call' | 'error' }

export interface ChatEntry {
  role: 'user' | 'assistant' | 'tool'
  content: string
}
