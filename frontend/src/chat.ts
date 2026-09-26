import type { Chat } from './types'

export const GENERAL_CHAT: Chat = { kind: 'general' }

export function taskChat(taskId: number): Chat {
  return { kind: 'task', task_id: taskId }
}

// 会話ごとの状態(応答待ち・途中経過・失敗・表示中の会話)を見分ける鍵。オブジェクトの
// 同一性では比べられないため、同じ会話が同じ文字列になるようにする。
export function chatKey(chat: Chat): string {
  return chat.kind === 'general' ? 'general' : `task:${chat.task_id}`
}
