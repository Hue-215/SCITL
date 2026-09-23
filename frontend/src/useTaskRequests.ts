import { useCallback, useState } from 'react'
import type { PendingEntry } from './types'

// タスクに対するコマンド(送信・編集・再試行・削除)の実行中の状態と失敗を、タスクごとに持つ
// (Issue #152、#70のうち独立した応答待ちの前倒し分)。応答待ちのタスクを離れて別のタスクを
// 操作でき、戻ってきたときも応答待ちの表示と失敗がそのタスクに残る。同じタスクで2本目を
// 始めないのは画面の無効化だけに頼らず、core側でも断る(orchestration::TurnContext::generating)。

function without<T>(map: Record<number, T>, key: number): Record<number, T> {
  const next = { ...map }
  delete next[key]
  return next
}

export interface TaskRequests {
  /** 実行中のコマンドの楽観表示。実行中でなければ空。 */
  pendingOf: (taskId: number) => PendingEntry[]
  isBusy: (taskId: number) => boolean
  /** コマンド自体が失敗した理由。次にそのタスクでコマンドを始めるまで残る。 */
  failureOf: (taskId: number) => string | null
  /**
   * そのタスクの発言をDBから引き直したときに呼ぶ。コマンドはユーザー発言を最初に保存するので、
   * 引き直したあとは楽観表示のユーザー発言が二重になる。応答待ちの表示だけを残す。
   */
  reloaded: (taskId: number) => void
  /**
   * タスクに対するコマンドを1つ実行する。終わったら成否によらず`settle`で引き直し、
   * そのあとで楽観表示を外す(外してから引き直すと、一瞬だけ発言が消えて見える)。
   */
  run: (
    taskId: number,
    pending: PendingEntry[],
    command: () => Promise<unknown>,
    settle: (taskId: number) => Promise<void>,
  ) => Promise<void>
}

export function useTaskRequests(): TaskRequests {
  const [pending, setPending] = useState<Record<number, PendingEntry[]>>({})
  const [failures, setFailures] = useState<Record<number, string>>({})

  const reloaded = useCallback((taskId: number) => {
    setPending((prev) =>
      prev[taskId] === undefined
        ? prev
        : { ...prev, [taskId]: prev[taskId].filter((entry) => entry.role !== 'user') },
    )
  }, [])

  const run: TaskRequests['run'] = async (taskId, entries, command, settle) => {
    setPending((prev) => ({ ...prev, [taskId]: entries }))
    setFailures((prev) => without(prev, taskId))
    try {
      await command()
    } catch (e) {
      setFailures((prev) => ({ ...prev, [taskId]: `操作に失敗しました: ${String(e)}` }))
    }
    await settle(taskId)
    setPending((prev) => without(prev, taskId))
  }

  return {
    pendingOf: (taskId) => pending[taskId] ?? [],
    isBusy: (taskId) => pending[taskId] !== undefined,
    failureOf: (taskId) => failures[taskId] ?? null,
    reloaded,
    run,
  }
}
