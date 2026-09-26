import { useCallback, useRef, useState } from 'react'
import { failureText } from './api'
import { chatKey } from './chat'
import { t } from './i18n'
import { appendTurnEvent, NO_LIVE_THOUGHTS, type LiveThoughts, type ThoughtItem } from './thinking'
import type { Chat, PendingEntry, TurnEvent } from './types'

// 会話に対するコマンド(送信・編集・再試行・削除)の実行中の状態と失敗を、会話ごとに持つ
// (Issue #152・#70)。応答待ちの会話を離れて別の会話を操作でき、戻ってきたときも応答待ちの
// 表示・途中経過・失敗がその会話に残る。同じ会話で2本目を始めないのは画面の無効化だけに
// 頼らず、core側でも断る(orchestration::TurnContext::generating)。

function without<T>(map: Record<string, T>, key: string): Record<string, T> {
  const next = { ...map }
  delete next[key]
  return next
}

export interface ChatRequests {
  /** 実行中のコマンドの楽観表示。実行中でなければ空。 */
  pendingOf: (chat: Chat) => PendingEntry[]
  /** 応答待ちの間に届いた思考・ツールの項目。 */
  liveOf: (chat: Chat) => ThoughtItem[]
  isBusy: (chat: Chat) => boolean
  /** コマンド自体が失敗した理由。次にその会話でコマンドを始めるまで残る。 */
  failureOf: (chat: Chat) => string | null
  /**
   * その会話の発言をDBから引き直したときに呼ぶ。コマンドはユーザー発言を最初に保存するので、
   * 引き直したあとは楽観表示のユーザー発言が二重になる。応答待ちの表示だけを残す。途中経過は
   * 残す(応答を生成中の試行は、DBから引いても出てこない)。
   */
  reloaded: (chat: Chat) => void
  /**
   * 会話に対するコマンドを1つ実行する。`command`には途中経過の受け口を渡す。終わったら成否に
   * よらず`settle`で引き直し、そのあとで楽観表示と途中経過を外す(外してから引き直すと、一瞬だけ
   * 発言が消えて見える)。
   */
  run: (
    chat: Chat,
    pending: PendingEntry[],
    command: (onEvent: (event: TurnEvent) => void) => Promise<unknown>,
    settle: (chat: Chat) => Promise<void>,
  ) => Promise<void>
}

export function useChatRequests(): ChatRequests {
  const [pending, setPending] = useState<Record<string, PendingEntry[]>>({})
  const [live, setLive] = useState<Record<string, LiveThoughts>>({})
  const [failures, setFailures] = useState<Record<string, string>>({})
  // 実行中のコマンドの印。途中経過はコマンドの完了より後に届きうるので、完了した
  // (印が外れた)コマンドの分は捨てる。
  const running = useRef<Record<string, object>>({})

  const reloaded = useCallback((chat: Chat) => {
    const key = chatKey(chat)
    setPending((prev) =>
      prev[key] === undefined
        ? prev
        : { ...prev, [key]: prev[key].filter((entry) => entry.role !== 'user') },
    )
  }, [])

  const run: ChatRequests['run'] = async (chat, entries, command, settle) => {
    const key = chatKey(chat)
    const token = {}
    running.current[key] = token
    setPending((prev) => ({ ...prev, [key]: entries }))
    setFailures((prev) => without(prev, key))
    const onEvent = (event: TurnEvent) => {
      if (running.current[key] !== token) return
      setLive((prev) => ({
        ...prev,
        [key]: appendTurnEvent(prev[key] ?? NO_LIVE_THOUGHTS, event),
      }))
    }
    try {
      await command(onEvent)
    } catch (e) {
      setFailures((prev) => ({
        ...prev,
        [key]: t('chat.command_failed', { error: failureText(e) }),
      }))
    }
    // 引き直しの最中に届いた分を積まないよう、引き直す前に外す。
    running.current = without(running.current, key)
    await settle(chat)
    setPending((prev) => without(prev, key))
    setLive((prev) => without(prev, key))
  }

  return {
    pendingOf: (chat) => pending[chatKey(chat)] ?? [],
    liveOf: (chat) => live[chatKey(chat)]?.items ?? [],
    isBusy: (chat) => pending[chatKey(chat)] !== undefined,
    failureOf: (chat) => failures[chatKey(chat)] ?? null,
    reloaded,
    run,
  }
}
