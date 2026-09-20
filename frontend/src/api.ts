import { invoke } from '@tauri-apps/api/core'
import type { ResponseEvent, Task } from './types'

// フロントエンドはIPCコマンドを呼ぶだけに徹する(DB・秘密情報・外部通信は持たない)。
// docs/spec/rebuild/architecture.md 7節。
export function getTaskDetail(taskId: number): Promise<Task> {
  return invoke('get_task_detail', { taskId })
}

export function sendTaskChatMessage(
  taskId: number,
  text: string,
): Promise<ResponseEvent[]> {
  return invoke('send_task_chat_message', { taskId, text })
}
