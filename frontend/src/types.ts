// 画面とRust側でやり取りする型。Rust側の型を正本とし、`bindings/`は`cargo test`がそこから
// 生成する(手で直さない)。ここは生成した型の入口と、画面だけが使う型を持つ。
export type { ApiFormat } from './bindings/ApiFormat'
export type { AttachmentDeliveries } from './bindings/AttachmentDeliveries'
export type { AttachmentKind } from './bindings/AttachmentKind'
export type { AttachmentView } from './bindings/AttachmentView'
export type { AvailableModel } from './bindings/AvailableModel'
export type { Capability } from './bindings/Capability'
export type { Chat } from './bindings/Chat'
export type { ChatModelsView } from './bindings/ChatModelsView'
export type { DataDirError } from './bindings/DataDirError'
export type { Delivery } from './bindings/Delivery'
export type { ExportSummary } from './bindings/ExportSummary'
export type { FinishReason } from './bindings/FinishReason'
export type { GeneralSettingsView } from './bindings/GeneralSettingsView'
export type { Kind } from './bindings/Kind'
export type { Language } from './bindings/Language'
export type { LinkInspection } from './bindings/LinkInspection'
export type { LinkVerdict } from './bindings/LinkVerdict'
export type { McpEndpointView } from './bindings/McpEndpointView'
export type { McpServerView } from './bindings/McpServerView'
export type { McpToolView } from './bindings/McpToolView'
export type { Message } from './bindings/Message'
export type { MessageView } from './bindings/MessageView'
export type { ModelCapabilities } from './bindings/ModelCapabilities'
export type { ModelChoice } from './bindings/ModelChoice'
export type { ModelView } from './bindings/ModelView'
export type { NewMcpEndpoint } from './bindings/NewMcpEndpoint'
export type { PartView } from './bindings/PartView'
export type { DropNotice } from './bindings/DropNotice'
export type { PickingLimits } from './bindings/PickingLimits'
export type { ProviderView } from './bindings/ProviderView'
export type { ReasoningEffort } from './bindings/ReasoningEffort'
export type { Rejection } from './bindings/Rejection'
export type { ResponseEvent } from './bindings/ResponseEvent'
export type { Role } from './bindings/Role'
export type { SelectedModel } from './bindings/SelectedModel'
export type { SettingsView } from './bindings/SettingsView'
export type { StageOutcome } from './bindings/StageOutcome'
export type { Task } from './bindings/Task'
export type { TaskCreation } from './bindings/TaskCreation'
export type { TaskDetailView } from './bindings/TaskDetailView'
export type { TaskListItem } from './bindings/TaskListItem'
export type { TaskSummary } from './bindings/TaskSummary'
export type { ToolArguments } from './bindings/ToolArguments'
export type { ToolExecutionView } from './bindings/ToolExecutionView'
export type { ToolSettingsView } from './bindings/ToolSettingsView'
export type { TurnEvent } from './bindings/TurnEvent'

// 送信直後の楽観表示専用のプレースホルダ。確定後はlist_chat_messagesで引き直して置き換える。
export interface PendingEntry {
  role: 'user' | 'pending'
  content: string
  // 送った添付の名前(ユーザー発言のみ)。確定するまで開けないので、名前だけを出す。
  attachmentNames?: string[]
  // 止める指示を出したあとの応答待ち(`content`は止めていることを伝える文言)。
  stopping?: boolean
}
