import { Channel, invoke } from '@tauri-apps/api/core'
import type {
  ApiFormat,
  BaseUrlHint,
  AvailableModel,
  Capability,
  Chat,
  ChatModelsView,
  DataDirError,
  ExportOutcome,
  ExportTarget,
  FormOutcome,
  Language,
  McpServerAdded,
  Memory,
  MessageView,
  NewMcpEndpoint,
  ReasoningEffort,
  ReceivedFiles,
  Regeneration,
  SettingsView,
  StageOutcome,
  TaskCreation,
  TaskDetailView,
  TaskListItem,
  TaskOpeningEvent,
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

// 作ったら続けて聞き取りを始める。作ったタスクと聞き取りの途中経過は、この順で`onEvent`へ
// 届く。返るのは聞き取りが終わってからで、返ったときに作った知らせがまだ届いていないこともある。
export function createTask(onEvent: (event: TaskOpeningEvent) => void): Promise<TaskCreation> {
  return invoke('create_task', { onEvent: new Channel(onEvent) })
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
// 編集・再試行は、チャットを使えない間(モデル未選択等)は何も消さずに断る(`unavailable`)。
// `attachments`は`stageReceivedFile`が返したトークン。
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
export function discardStagedAttachment(token: string): Promise<void> {
  return invoke('discard_staged_attachment', { token })
}

// 送っていない添付をすべて捨てる。画面は起動のたびに呼ぶ(読み込み直した画面は入力欄の添付を
// 持たないので、Rust側に残った預かりが数の上限を埋めないように)。
export function discardAllStagedAttachments(): Promise<void> {
  return invoke('discard_all_staged_attachments')
}

// 添付になるファイルは、画面を通らずにRust側がOSから受け取る(窓に落とした・選択画面で選んだ・
// クリップボードの画像)。画面には名前だけが知らされ、受け付けたものだけを、受け取りの番号と
// 並びの位置で指して読ませる。

// 窓に落としたファイルの知らせ先。1つで、渡し直すと置き換わる。
export function watchDroppedFiles(onDrop: (files: ReceivedFiles) => void): Promise<void> {
  return invoke('watch_dropped_files', { onDrop: new Channel(onDrop) })
}

// 通知を押して開くことになった会話の知らせ先(Android)。1つで、渡し直すと置き換わる。どの会話を
// 開くかはRust側が確かめて決め、画面はそれを表示するだけ。
export function watchRequestedChats(onRequest: (chat: Chat) => void): Promise<void> {
  return invoke('watch_requested_chats', { onRequest: new Channel(onRequest) })
}

// OSの選択画面で選ばせる。取りやめたらnull。
export function pickAttachments(): Promise<ReceivedFiles | null> {
  return invoke('pick_attachments')
}

// 文字の無い貼り付けが起きたことを伝え、クリップボードの画像を受け取らせる。画像が無ければnull。
export function pasteClipboardImage(): Promise<ReceivedFiles | null> {
  return invoke('paste_clipboard_image')
}

// 受け取ったファイルの1つを読ませて預ける。種別・大きさ・1つの発言に付けられる数の判定はRust側。
export function stageReceivedFile(batchId: number, index: number): Promise<StageOutcome> {
  return invoke('stage_received_file', { batchId, index })
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

// 書き出し先は画面からは選ばない(選ぶOSでは、Rust側が保存画面を出す)。
export function getExportTarget(): Promise<ExportTarget> {
  return invoke('get_export_target')
}

export function exportMarkdown(): Promise<ExportOutcome> {
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
): Promise<Regeneration> {
  return invoke('edit_chat_message', { chat, messageId, text, onEvent: new Channel(onEvent) })
}

export function retryChatMessage(
  chat: Chat,
  messageId: number,
  onEvent: (event: TurnEvent) => void,
): Promise<Regeneration> {
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
// 数値の欄(`responseTimeoutSecs`等)は入力欄の文字列のまま送る。空欄は「未設定」で、Rust側の
// 既定値に戻る。nullは保存済みの値のまま変えない(書き換えた欄だけを送る)。解釈と検証は
// Rust側が行い、欄の誤りは`rejected`で返る。
export function updateGeneralSettings(args: {
  systemPrompt: string | null
  taskChatSystemPrompt: string | null
  taskOpeningMessage: string | null
  responseTimeoutSecs: string | null
}): Promise<FormOutcome<SettingsView>> {
  return invoke('update_general_settings', args)
}

// ツール呼び出しの上限。数値の欄の扱いはupdateGeneralSettingsと同じ。引数の取り違えを
// 避けるためオブジェクト引数にする。
export function updateToolSettings(args: {
  maxRoundsPerTurn: string | null
  totalTimeoutSecs: string | null
}): Promise<FormOutcome<SettingsView>> {
  return invoke('update_tool_settings', args)
}

// `headers`はヘッダーの欄の文字列のまま送る(1行1件の解釈はRust側)。
export function addProvider(
  name: string,
  apiFormat: ApiFormat,
  baseUrl: string,
  apiKey: string | null,
  headers: string,
): Promise<FormOutcome<SettingsView>> {
  return invoke('add_provider', { name, apiFormat, baseUrl, apiKey, headers })
}

// 入力中のベースURLへのヒント(版のパスまで書いた等)。判定はRust側(アダプタの知識)。
export function getBaseUrlHint(apiFormat: ApiFormat, baseUrl: string): Promise<BaseUrlHint | null> {
  return invoke('get_base_url_hint', { apiFormat, baseUrl })
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

// `contextLength`は入力欄の文字列のまま送る(空欄は手動設定を外す)。
export function setModelContextLength(
  providerId: string,
  model: string,
  contextLength: string,
): Promise<FormOutcome<SettingsView>> {
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
export function addMcpServer(
  name: string,
  endpoint: NewMcpEndpoint,
): Promise<FormOutcome<McpServerAdded>> {
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

// 本文中のリンクを開きたいと伝える。判定も、開く前の確認(ネイティブのダイアログ)も、開けな
// かったときの知らせもRust側が行い、返るのはダイアログを閉じてから。
export function openLink(url: string): Promise<void> {
  return invoke('open_link', { url })
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
