import { useCallback, useRef, useState } from 'react'
import { failureText } from './api'
import { t } from './i18n'
import { appendTurnEvent, NO_LIVE_THOUGHTS, type LiveThoughts, type ThoughtItem } from './thinking'
import type { PendingEntry, TurnEvent } from './types'

// タスクに対するコマンド(送信・編集・再試行・削除)の実行中の状態と失敗を、タスクごとに持つ
// (Issue #152・#70)。応答待ちのタスクを離れて別のタスクを操作でき、戻ってきたときも応答待ちの
// 表示・途中経過・失敗がそのタスクに残る。同じタスクで2本目を始めないのは画面の無効化だけに
// 頼らず、core側でも断る(orchestration::TurnContext::generating)。

function without<T>(map: Record<number, T>, key: number): Record<number, T> {
  const next = { ...map }
  delete next[key]
  return next
}

export interface TaskRequests {
  /** 実行中のコマンドの楽観表示。実行中でなければ空。 */
  pendingOf: (taskId: number) => PendingEntry[]
  /** 応答待ちの間に届いた思考・ツールの項目。 */
  liveOf: (taskId: number) => ThoughtItem[]
  isBusy: (taskId: number) => boolean
  /** コマンド自体が失敗した理由。次にそのタスクでコマンドを始めるまで残る。 */
  failureOf: (taskId: number) => string | null
  /**
   * そのタスクの発言をDBから引き直したときに呼ぶ。コマンドはユーザー発言を最初に保存するので、
   * 引き直したあとは楽観表示のユーザー発言が二重になる。応答待ちの表示だけを残す。途中経過は
   * 残す(応答を生成中の試行は、DBから引いても出てこない)。
   */
  reloaded: (taskId: number) => void
  /**
   * タスクに対するコマンドを1つ実行する。`command`には途中経過の受け口を渡す。終わったら成否に
   * よらず`settle`で引き直し、そのあとで楽観表示と途中経過を外す(外してから引き直すと、一瞬だけ
   * 発言が消えて見える)。
   */
  run: (
    taskId: number,
    pending: PendingEntry[],
    command: (onEvent: (event: TurnEvent) => void) => Promise<unknown>,
    settle: (taskId: number) => Promise<void>,
  ) => Promise<void>
}

export function useTaskRequests(): TaskRequests {
  const [pending, setPending] = useState<Record<number, PendingEntry[]>>({})
  const [live, setLive] = useState<Record<number, LiveThoughts>>({})
  const [failures, setFailures] = useState<Record<number, string>>({})
  // 実行中のコマンドの印。途中経過はコマンドの完了より後に届きうるので、完了した
  // (印が外れた)コマンドの分は捨てる。
  const running = useRef<Record<number, object>>({})

  const reloaded = useCallback((taskId: number) => {
    setPending((prev) =>
      prev[taskId] === undefined
        ? prev
        : { ...prev, [taskId]: prev[taskId].filter((entry) => entry.role !== 'user') },
    )
  }, [])

  const run: TaskRequests['run'] = async (taskId, entries, command, settle) => {
    const token = {}
    running.current[taskId] = token
    setPending((prev) => ({ ...prev, [taskId]: entries }))
    setFailures((prev) => without(prev, taskId))
    const onEvent = (event: TurnEvent) => {
      if (running.current[taskId] !== token) return
      setLive((prev) => ({
        ...prev,
        [taskId]: appendTurnEvent(prev[taskId] ?? NO_LIVE_THOUGHTS, event),
      }))
    }
    try {
      await command(onEvent)
    } catch (e) {
      setFailures((prev) => ({
        ...prev,
        [taskId]: t('chat.command_failed', { error: failureText(e) }),
      }))
    }
    // 引き直しの最中に届いた分を積まないよう、引き直す前に外す。
    running.current = without(running.current, taskId)
    await settle(taskId)
    setPending((prev) => without(prev, taskId))
    setLive((prev) => without(prev, taskId))
  }

  return {
    pendingOf: (taskId) => pending[taskId] ?? [],
    liveOf: (taskId) => live[taskId]?.items ?? [],
    isBusy: (taskId) => pending[taskId] !== undefined,
    failureOf: (taskId) => failures[taskId] ?? null,
    reloaded,
    run,
  }
}
