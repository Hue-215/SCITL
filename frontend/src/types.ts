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

// crates/scitl-core/src/orchestration/turn.rs の TaskCreation。チャットを使えない間は
// タスクを作らず、その理由をエラー発言と同じ種別コードで返す。
export type TaskCreation =
  | { status: 'created'; task: Task }
  | { status: 'unavailable'; error_kind: string }

// crates/scitl-core/src/db/tasks.rs の TaskDetailView(Taskをフラット化したもの)。
// ヘッダー向け。fallback_labelの意味はTaskSummaryと同じ。
export interface TaskDetail extends Task {
  fallback_label: string | null
}

// crates/scitl-core/src/db/tasks.rs の TaskListItem(TaskSummaryをフラット化したもの)。
export interface TaskSummary {
  id: number
  title: string | null
  deadline: string | null
  archived_at: string | null
  steps_done: number
  steps_total: number
  // titleが未設定のときに一覧で代わりに出す、最初のユーザー発言の切り詰め(Issue #61)。
  // 表示専用で、tasks.titleには書き込まれない。発言がまだ無ければnull。
  fallback_label: string | null
}

// crates/scitl-core/src/llm/mod.rs の ToolArguments と一致させる。
export type ToolArguments =
  | { status: 'valid'; value: unknown }
  | { status: 'malformed'; raw: string; error: string }

export type ResponseEvent =
  | { type: 'text_delta'; text: string }
  // モデルの思考(reasoning)の断片(Issue #42)。表示・保存専用で、APIへの再送信には
  // 使わない(docs/spec/principles.md 3節)。
  | { type: 'reasoning_delta'; text: string }
  | { type: 'tool_call'; id: string | null; name: string; arguments: ToolArguments }
  | { type: 'done'; finish_reason: 'stop' | 'tool_call' | 'length' | 'error' }

// crates/scitl-core/src/orchestration/tool_record.rs の ToolExecutionRecord と一致させる。
// ツール実行記録の行の`content`と同じ形。
export interface ToolExecutionRecord {
  tool: string
  arguments: unknown
  result: unknown
  tool_kind?: 'state' | 'fact'
  call_id?: string
}

// crates/scitl-core/src/orchestration/turn_event.rs の TurnEvent と一致させる。
// 応答待ちの間の表示専用で、完了したらDBから読み直した発言に置き換わる。
export type TurnEvent =
  | { type: 'response'; event: ResponseEvent }
  // `id`は保存したツール実行記録の行のid
  | { type: 'tool_executed'; id: number; record: ToolExecutionRecord }

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
  // 画面の「詳細を表示」専用。プロバイダーの応答本文を含みうる(data-model.md messages)。
  error_detail: string | null
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

// crates/scitl-core/src/i18n.rs の Language。
export type Language = 'ja' | 'en'

// crates/scitl-core/src/settings/view.rs の型と一致させる。
export type ApiFormat = 'open_ai_compat'

export interface GeneralSettings {
  system_prompt: string | null
  task_chat_system_prompt: string | null
  task_opening_message: string | null
  response_timeout_secs: number | null
  // 未設定時に実際に使われる値(ToolSettingsのdefault_*と同じ扱い)。
  default_task_chat_system_prompt: string
  default_task_opening_message: string
  default_response_timeout_secs: number
  // 保存した表示言語(未設定なら既定の言語に解決済み)。画面は起動時の言語で描かれている。
  language: Language
}

// ツール呼び出しの上限(Issue #71)。default_*は未設定時に実際に使われる値で、
// Rust側(orchestration::ToolLimits)が持つものをそのまま受け取る。プレースホルダに
// 出すだけなので、ここで既定値を定義し直さない。
export interface ToolSettings {
  max_rounds_per_turn: number | null
  total_timeout_secs: number | null
  default_max_rounds_per_turn: number
  default_total_timeout_secs: number
}

// crates/scitl-core/src/config.rs の Capability。
export type Capability = 'image' | 'tools' | 'thinking'

// crates/scitl-core/src/llm/capabilities.rs の ModelCapabilities。解決済みの値。
export interface ModelCapabilities {
  image: boolean
  tools: boolean
  thinking: boolean
  context_length: number
}

// crates/scitl-core/src/settings/view.rs の ModelView。
export interface ModelView {
  // 操作の鍵として送り返す名前。画面に出すのはlabel。
  name: string
  // 見えない文字を除いた、画面に出す名前(architecture.md 10節)。
  label: string
  visible: boolean
  capabilities: ModelCapabilities
  // 手動設定が無いとき(自動検出 → 既定値)のコンテキスト長。
  default_context_length: number
  // 能力に手動設定がある(「初期値に戻す」を出す)。
  overridden: boolean
}

export interface ProviderView {
  id: string
  name: string
  api_format: ApiFormat
  base_url: string
  models: ModelView[]
  active_model: string | null
  has_api_key: boolean
  // モデルの能力を推論サーバーに問い合わせられる(「能力を検出」を出す)。
  can_detect_capabilities: boolean
  // このプロバイダーをアクティブにしているが、組み立てられない理由。
  error: string | null
}

// crates/scitl-core/src/config.rs の ReasoningEffort。
export type ReasoningEffort = 'off' | 'low' | 'medium' | 'high'

// crates/scitl-core/src/settings/view.rs の ChatModelsView(チャット入力欄の下のモデル選択)。
export interface ModelChoice {
  provider_id: string
  provider_name: string
  // 選ぶときに送り返す名前。画面に出すのはlabel(ModelViewと同じ)。
  model: string
  label: string
}

// crates/scitl-core/src/settings/view.rs の AvailableModel(プロバイダーから取得したモデル)。
export interface AvailableModel {
  // サーバーが返したままの名前。登録するときに送り返す。
  name: string
  label: string
}

export interface SelectedModel extends ModelChoice {
  // 思考に対応する(3層で解決済み)。対応しなければ思考の強さは選べない。
  thinking: boolean
  reasoning_effort: ReasoningEffort
}

export interface ChatModelsView {
  // 設定画面で表示にしたモデル。
  choices: ModelChoice[]
  // 一覧から隠したモデルでも、使っていれば入る。
  selected: SelectedModel | null
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
  // 画面に出すツール一覧。未取得なら有効化済みのツールだけが説明なしで入る(Issue #104)。
  // 取得結果はRust側のキャッシュが持ち、画面はそれを描くだけ。
  tools: McpToolView[]
  tools_fetched: boolean
}

// crates/scitl-core/src/settings/view.rs の McpToolView。
export interface McpToolView {
  // サーバーが返したままの名前。有効化を切り替えるときの鍵にだけ使い、画面には描かない。
  name: string
  label: string
  description: string | null
  exposable: boolean
}

export interface SettingsView {
  // 起動時に設定ファイルを読めなかった理由。あれば設定は保存されない。
  config_error: string | null
  general: GeneralSettings
  tools: ToolSettings
  providers: ProviderView[]
  active_provider_id: string | null
  mcp_servers: McpServerView[]
}

// crates/scitl-core/src/link.rs の LinkVerdict / LinkInspection。
export type LinkVerdict =
  | { kind: 'web' }
  | { kind: 'mail' }
  | { kind: 'unreadable' }
  | { kind: 'scheme_blocked'; scheme: string }

export interface LinkInspection {
  url: string
  verdict: LinkVerdict
  can_open: boolean
  real_url: string | null
  userinfo_host: string | null
}
