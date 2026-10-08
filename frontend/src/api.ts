import { Channel, invoke } from '@tauri-apps/api/core'
import type {
  ApiFormat,
  PickingLimits,
  AvailableModel,
  Capability,
  Chat,
  ChatModelsView,
  DataDirError,
  DropNotice,
  ExportSummary,
  Language,
  LinkInspection,
  McpServerAdded,
  Memory,
  MessageView,
  NewMcpEndpoint,
  ReasoningEffort,
  SettingsView,
  StageOutcome,
  Task,
  TaskCreation,
  TaskDetailView,
  TaskListItem,
  TurnEvent,
} from './types'

// フロントエンドはIPCコマンドを呼ぶだけに徹する(DB・秘密情報・外部通信は持たない)。

/**
 * コマンドの失敗を画面に出す文字列にする唯一の入口。今はcoreのエラー文(英語の診断文)
 * をそのまま返し、表示言語には訳していない。
 */
// TODO(#199): 表示言語に訳す。
export function failureText(e: unknown): string {
  return String(e)
}
export function getTaskDetail(taskId: number): Promise<TaskDetailView> {
  return invoke('get_task_detail', { taskId })
}

export function listTasks(): Promise<TaskListItem[]> {
  return invoke('list_tasks')
}

// 作ったら続けて聞き取りを始める。作ったタスクは聞き取りの前に`onCreated`へ、聞き取りの
// 途中経過は`onEvent`へ届き、返るのは聞き取りが終わってから。
export function createTask(
  onCreated: (task: Task) => void,
  onEvent: (event: TurnEvent) => void,
): Promise<TaskCreation> {
  return invoke('create_task', {
    onCreated: new Channel(onCreated),
    onEvent: new Channel(onEvent),
  })
}

// ヘッダーからのタスク操作。どれも応答を生成中のタスクでは断られる。
export function renameTask(taskId: number, title: string): Promise<void> {
  return invoke('rename_task', { taskId, title })
}

export function setTaskArchived(taskId: number, archived: boolean): Promise<void> {
  return invoke('set_task_archived', { taskId, archived })
}

export function deleteTask(taskId: number): Promise<void> {
  return invoke('delete_task', { taskId })
}

// 送信・編集・再試行は、ターンの途中経過を`onEvent`へ届ける。経路(Channel)はコマンドの
// 呼び出しごとに作るので、届いたイベントがどの会話のものかは呼び出し側が知っている。
// `attachments`は`stageAttachment`が返したトークン。
export function sendChatMessage(
  chat: Chat,
  text: string,
  attachments: string[],
  onEvent: (event: TurnEvent) => void,
): Promise<void> {
  return invoke('send_chat_message', {
    chat,
    text,
    attachments,
    onEvent: new Channel(onEvent),
  })
}

// 添付。中身は生のバイト列で送り、名前はヘッダーに載せる(ヘッダーはASCIIしか
// 運べないので符号化する)。パスを渡すコマンドは無い。
export async function stageAttachment(file: File): Promise<StageOutcome> {
  const bytes = new Uint8Array(await file.arrayBuffer())
  return invoke('stage_attachment', bytes, {
    headers: { 'x-scitl-file-name': encodeURIComponent(file.name) },
  })
}

export function discardStagedAttachment(token: string): Promise<void> {
  return invoke('discard_staged_attachment', { token })
}

// 窓に落としたファイル。パスはOSからRust側へ直接届き、画面には名前だけが知らされる。知らせ先は
// 1つで、渡し直すと置き換わる。受け付けたものだけを、ドロップの番号と並びの位置で指して読ませる。
export function watchDroppedFiles(onDrop: (notice: DropNotice) => void): Promise<void> {
  return invoke('watch_dropped_files', { onDrop: new Channel(onDrop) })
}

export function stageDroppedFile(dropId: number, index: number): Promise<StageOutcome> {
  return invoke('stage_dropped_file', { dropId, index })
}

export function getAttachmentLimits(): Promise<PickingLimits> {
  return invoke('get_attachment_limits')
}

export function readTextAttachment(attachmentId: number): Promise<string> {
  return invoke('read_text_attachment', { attachmentId })
}

// data URL(`data:image/…;base64,`で始まることはRust側が保証する)。
export function readImageAttachment(attachmentId: number): Promise<string> {
  return invoke('read_image_attachment', { attachmentId })
}

export function revealAttachment(attachmentId: number): Promise<void> {
  return invoke('reveal_attachment', { attachmentId })
}

// 書き出し先は画面からは選ばない。
export function exportMarkdown(): Promise<ExportSummary> {
  return invoke('export_markdown')
}

export function openExportFolder(): Promise<void> {
  return invoke('open_export_folder')
}

export function listChatMessages(chat: Chat): Promise<MessageView[]> {
  return invoke('list_chat_messages', { chat })
}

// 編集・再試行・削除。いずれも対象は`messageId`で指定し、会話の取り違え防止のため`chat`も
// 渡す。
export function editChatMessage(
  chat: Chat,
  messageId: number,
  text: string,
  onEvent: (event: TurnEvent) => void,
): Promise<void> {
  return invoke('edit_chat_message', { chat, messageId, text, onEvent: new Channel(onEvent) })
}

export function retryChatMessage(
  chat: Chat,
  messageId: number,
  onEvent: (event: TurnEvent) => void,
): Promise<void> {
  return invoke('retry_chat_message', { chat, messageId, onEvent: new Channel(onEvent) })
}

// 返信の無いまま終わった会話に、応答を生成する。返信(エラー発言を含む)で終わる会話では
// 断られる(エラー発言は`retryChatMessage`で作り直す)。
export function generateChatReply(
  chat: Chat,
  onEvent: (event: TurnEvent) => void,
): Promise<void> {
  return invoke('generate_chat_reply', { chat, onEvent: new Channel(onEvent) })
}

// 会話が返信の無いまま終わっているか(`generateChatReply`を受け付けるか)。
export function chatLacksReply(chat: Chat): Promise<boolean> {
  return invoke('chat_lacks_reply', { chat })
}

// 生成中の応答を止める。生成中でなければ何もせず`false`。止めたターンは、生成を始めた
// コマンドが終わったときには保存されている。
export function stopChatResponse(chat: Chat): Promise<boolean> {
  return invoke('stop_chat_response', { chat })
}

// 削除は、対象とそれより後ろの発言をまとめて消す。
export function deleteChatMessage(chat: Chat, messageId: number): Promise<void> {
  return invoke('delete_chat_message', { chat, messageId })
}

export function getSettings(): Promise<SettingsView> {
  return invoke('get_settings')
}

// 起動時にデータフォルダを開けなかった理由。開けていればnull。このときは他のコマンドを呼ばない。
export function getStartupFailure(): Promise<DataDirError | null> {
  return invoke('get_startup_failure')
}

// 起動時に1度だけ読む(切り替えは再起動で反映する)。
export function getDisplayLanguage(): Promise<Language> {
  return invoke('get_display_language')
}

export function updateLanguage(language: Language): Promise<SettingsView> {
  return invoke('update_language', { language })
}

// プロンプトはどれも`string | null`で並ぶため、取り違えないようオブジェクト引数にする。
export function updateGeneralSettings(args: {
  systemPrompt: string | null
  taskChatSystemPrompt: string | null
  taskOpeningMessage: string | null
  responseTimeoutSecs: number | null
}): Promise<SettingsView> {
  return invoke('update_general_settings', args)
}

// ツール呼び出しの上限。nullは「未設定」で、Rust側の既定値に戻る。updateGeneralSettingsと
// 同じ理由(number | nullが並ぶ)でオブジェクト引数にする。
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
  headers: [string, string][],
): Promise<SettingsView> {
  return invoke('add_provider', { name, apiFormat, baseUrl, apiKey, headers })
}

export function deleteProvider(providerId: string): Promise<SettingsView> {
  return invoke('delete_provider', { providerId })
}

// 1件でも登録できない名前があれば、1件も登録しない。
// 登録したら、検出できるプロバイダーなら続けて能力を検出する(検出の失敗は追加の失敗にしない)。
export function addModels(providerId: string, models: string[]): Promise<SettingsView> {
  return invoke('add_models', { providerId, models })
}

// プロバイダーが提供するモデル名(登録済みのものも含む)。登録はaddModelsで行う。
export function listProviderModels(providerId: string): Promise<AvailableModel[]> {
  return invoke('list_provider_models', { providerId })
}

export function removeModel(providerId: string, model: string): Promise<SettingsView> {
  return invoke('remove_model', { providerId, model })
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

// チャット入力欄の下のモデル選択。selectChatModelで選び直した後は、これで一覧を引き直す。
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

// 登録したら続けてツール一覧を取得する。取得に失敗しても登録は残り、理由が添えられる。
export function addMcpServer(name: string, endpoint: NewMcpEndpoint): Promise<McpServerAdded> {
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

// 取得した一覧はRust側のキャッシュに載り、設定画面の状態ごと返ってくる。
export function fetchMcpTools(serverId: string): Promise<SettingsView> {
  return invoke('fetch_mcp_tools', { serverId })
}

// 本文中のリンク。開く側でもRustが判定し直すため、確認ダイアログを経ずにopenConfirmedLinkを
// 呼んでも許可されないURLは開かない。
export function inspectLink(url: string): Promise<LinkInspection> {
  return invoke('inspect_link', { url })
}

export function openConfirmedLink(url: string): Promise<void> {
  return invoke('open_confirmed_link', { url })
}

// 設定のメモリタブ。変更のコマンドは何も返さないので、画面は続けて一覧を読み直す。
export function listMemories(): Promise<Memory[]> {
  return invoke('list_memories')
}

// 空・長すぎる本文と、上限の件数を超える追加は断られる。新しく足したメモリを返し、既にある
// 本文と同じなら空の配列になる。
export function addMemory(content: string): Promise<Memory[]> {
  return invoke('add_memory', { content })
}

export function updateMemory(memoryId: number, content: string): Promise<void> {
  return invoke('update_memory', { memoryId, content })
}

export function deleteMemory(memoryId: number): Promise<void> {
  return invoke('delete_memory', { memoryId })
}
