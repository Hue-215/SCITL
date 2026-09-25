import { invoke } from '@tauri-apps/api/core'
import type {
  ApiFormat,
  Capability,
  ChatModelsView,
  LinkInspection,
  Message,
  ReasoningEffort,
  ResponseEvent,
  SettingsView,
  Task,
  TaskDetail,
  TaskSummary,
} from './types'

// フロントエンドはIPCコマンドを呼ぶだけに徹する(DB・秘密情報・外部通信は持たない)。
// docs/spec/rebuild/architecture.md 7節。
export function getTaskDetail(taskId: number): Promise<TaskDetail> {
  return invoke('get_task_detail', { taskId })
}

export function listTasks(): Promise<TaskSummary[]> {
  return invoke('list_tasks')
}

export function createTask(): Promise<Task> {
  return invoke('create_task')
}

export function sendTaskChatMessage(
  taskId: number,
  text: string,
): Promise<ResponseEvent[]> {
  return invoke('send_task_chat_message', { taskId, text })
}

export function listTaskMessages(taskId: number): Promise<Message[]> {
  return invoke('list_task_messages', { taskId })
}

// 編集・再試行・削除(Issue #41)。いずれも対象は`messageId`で指定し、
// タスクの取り違え防止のため`taskId`も渡す(tools.md 1節と同じ理由)。
export function editTaskChatMessage(
  taskId: number,
  messageId: number,
  text: string,
): Promise<ResponseEvent[]> {
  return invoke('edit_task_chat_message', { taskId, messageId, text })
}

export function retryTaskChatMessage(
  taskId: number,
  messageId: number,
): Promise<ResponseEvent[]> {
  return invoke('retry_task_chat_message', { taskId, messageId })
}

export function deleteTaskChatMessage(taskId: number, messageId: number): Promise<void> {
  return invoke('delete_task_chat_message', { taskId, messageId })
}

export function getSettings(): Promise<SettingsView> {
  return invoke('get_settings')
}

// systemPrompt/taskChatSystemPromptはどちらも`string | null`で並ぶため、位置引数だと
// 呼び出し側での取り違えに気付きにくい(docs/spec/rebuild/tools.md 1節が修正した
// 「対象タスクの取り違え」と同種の事故)。名前で縛るためオブジェクト引数にする。
export function updateGeneralSettings(args: {
  systemPrompt: string | null
  taskChatSystemPrompt: string | null
  responseTimeoutSecs: number | null
}): Promise<SettingsView> {
  return invoke('update_general_settings', args)
}

// ツール呼び出しの上限(Issue #71)。nullは「未設定」で、Rust側の既定値に戻る。
// updateGeneralSettingsと同じ理由(number | nullが並ぶ)でオブジェクト引数にする。
export function updateToolSettings(args: {
  maxRoundsPerTurn: number | null
  totalTimeoutSecs: number | null
}): Promise<SettingsView> {
  return invoke('update_tool_settings', args)
}

export function addProvider(
  name: string,
  apiFormat: ApiFormat,
  baseUrl: string,
  apiKey: string | null,
): Promise<SettingsView> {
  return invoke('add_provider', { name, apiFormat, baseUrl, apiKey })
}

export function deleteProvider(providerId: string): Promise<SettingsView> {
  return invoke('delete_provider', { providerId })
}

export function setActiveProvider(providerId: string): Promise<SettingsView> {
  return invoke('set_active_provider', { providerId })
}

// 1件でも登録できない名前があれば、1件も登録しない。
export function addModels(providerId: string, models: string[]): Promise<SettingsView> {
  return invoke('add_models', { providerId, models })
}

export function removeModel(providerId: string, model: string): Promise<SettingsView> {
  return invoke('remove_model', { providerId, model })
}

export function setActiveModel(providerId: string, model: string): Promise<SettingsView> {
  return invoke('set_active_model', { providerId, model })
}

export function setModelVisible(
  providerId: string,
  model: string,
  visible: boolean,
): Promise<SettingsView> {
  return invoke('set_model_visible', { providerId, model, visible })
}

export function setModelCapability(
  providerId: string,
  model: string,
  capability: Capability,
  supported: boolean,
): Promise<SettingsView> {
  return invoke('set_model_capability', { providerId, model, capability, supported })
}

export function setModelContextLength(
  providerId: string,
  model: string,
  contextLength: number | null,
): Promise<SettingsView> {
  return invoke('set_model_context_length', { providerId, model, contextLength })
}

export function resetModelCapabilities(providerId: string, model: string): Promise<SettingsView> {
  return invoke('reset_model_capabilities', { providerId, model })
}

export function detectModelCapabilities(providerId: string): Promise<SettingsView> {
  return invoke('detect_model_capabilities', { providerId })
}

// チャット入力欄の下のモデル選択(Issue #64)。選び直した後は一覧を引き直す
// (能力の問い合わせを伴うため、Rust側で保存とは別のコマンドにしてある)。
export function getChatModels(): Promise<ChatModelsView> {
  return invoke('get_chat_models')
}

export function selectChatModel(providerId: string, model: string): Promise<void> {
  return invoke('select_chat_model', { providerId, model })
}

export function setReasoningEffort(
  providerId: string,
  model: string,
  effort: ReasoningEffort,
): Promise<void> {
  return invoke('set_reasoning_effort', { providerId, model, effort })
}

export type NewMcpEndpoint =
  | { transport: 'stdio'; command: string; args: string[]; env: [string, string][] }
  | { transport: 'streamable_http'; url: string; headers: [string, string][] }

export function addMcpServer(name: string, endpoint: NewMcpEndpoint): Promise<SettingsView> {
  return invoke('add_mcp_server', { name, endpoint })
}

export function deleteMcpServer(serverId: string): Promise<SettingsView> {
  return invoke('delete_mcp_server', { serverId })
}

export function setMcpServerEnabled(serverId: string, enabled: boolean): Promise<SettingsView> {
  return invoke('set_mcp_server_enabled', { serverId, enabled })
}

export function setMcpToolEnabled(
  serverId: string,
  toolName: string,
  enabled: boolean,
): Promise<SettingsView> {
  return invoke('set_mcp_tool_enabled', { serverId, toolName, enabled })
}

// 取得した一覧はRust側のキャッシュに載り、設定画面の状態ごと返ってくる(Issue #104)。
// フロントエンド側で一覧を保持しない(画面移動で消えるのを防ぐ)。
export function fetchMcpTools(serverId: string): Promise<SettingsView> {
  return invoke('fetch_mcp_tools', { serverId })
}

// 本文中のリンク(Issue #39)。開く側でもRustが判定し直すため、確認ダイアログを経ずに
// openConfirmedLinkを呼んでも許可されないURLは開かない(architecture.md 8節)。
export function inspectLink(url: string): Promise<LinkInspection> {
  return invoke('inspect_link', { url })
}

export function openConfirmedLink(url: string): Promise<void> {
  return invoke('open_confirmed_link', { url })
}
