import { useCallback, useRef, useState } from 'react'
import { failureText } from './api'
import { chatKey } from './chat'
import { isolated, t } from './i18n'
import { without } from './record'
import { appendTurnEvent, NO_LIVE_TURN, type LiveTurn, type TurnSegment } from './thinking'
import type { Chat, PendingEntry, TurnEvent } from './types'

// 会話に対するコマンド(聞き取りの開始・送信・編集・再試行・削除と、ヘッダーからのタスク操作)
// の実行中の状態と失敗を、会話ごとに持つ。応答待ちの会話を離れて別の会話を操作でき、
// 戻ってきたときも応答待ちの表示・途中経過・失敗・停止中の表示がその会話に残る。

export interface ChatRequests {
  /**
   * 実行中のコマンドの楽観表示。実行中でなければ空。止める指示を出したあとは、応答待ちの
   * 文言が停止中のものに変わる。
   */
  pendingOf: (chat: Chat) => PendingEntry[]
  /** 応答待ちの間に届いたターンの中身(思考・ツールの折りたたみと本文)。届いた順。 */
  liveOf: (chat: Chat) => TurnSegment[]
  isBusy: (chat: Chat) => boolean
  /** 応答を生成するコマンドの実行中(楽観表示に応答待ちがある)。削除・タスク操作の実行中は偽。 */
  isGenerating: (chat: Chat) => boolean
  /** 止める指示を出してから、そのコマンドが終わるまで真。 */
  isStopping: (chat: Chat) => boolean
  /** コマンド自体が失敗した理由。次にその会話でコマンドを始めるまで残る。 */
  failureOf: (chat: Chat) => string | null
  /**
   * その会話の発言をDBから引き直したときに、引き直した発言と同じ描画で呼ぶ。
   * コマンドの実行中なら、ユーザー発言は最初に保存されているので楽観表示のユーザー発言だけを
   * 外し、応答待ちの表示と途中経過は残す(応答を生成中の試行は、DBから引いても出てこない)。
   * コマンドが終わったあとなら、確定したターンと並ばないよう楽観表示も途中経過も外す。
   */
  reloaded: (chat: Chat) => void
  /**
   * 会話に対するコマンドを1つ実行する。`command`には途中経過の受け口を渡す。終わったら成否に
   * よらず`settle`で引き直す。楽観表示と途中経過は、表示中の会話なら引き直しと同時に
   * (`reloaded`)、そうでなければ`settle`のあとで外す(外してから引き直すと、一瞬だけ発言が
   * 消えて見える)。
   */
  run: (
    chat: Chat,
    pending: PendingEntry[],
    command: (onEvent: (event: TurnEvent) => void) => Promise<unknown>,
    settle: (chat: Chat) => Promise<void>,
  ) => Promise<void>
  /**
   * 生成中の応答を止める指示を出す(`command`)。止まったターンは、生成を始めたコマンドの
   * 完了とともに`run`の`settle`で引き直される。`command`が`false`を返したら(まだ生成が
   * 始まっていなかった等)止める指示を出していないので、停止中の表示を外す。
   */
  stop: (chat: Chat, command: () => Promise<boolean>) => Promise<void>
}

// 途中経過の無い会話に返す列。描画のたびに新しい配列を作ると、途中経過を見て追従する
// スクロールが毎回動くため、同じものを返す。
const NO_SEGMENTS: TurnSegment[] = []

export function useChatRequests(): ChatRequests {
  const [pending, setPending] = useState<Record<string, PendingEntry[]>>({})
  const [live, setLive] = useState<Record<string, LiveTurn>>({})
  const [failures, setFailures] = useState<Record<string, string>>({})
  const [stopping, setStopping] = useState<Record<string, true>>({})
  // 実行中のコマンドの印。途中経過はコマンドの完了より後に届きうるので、完了した
  // (印が外れた)コマンドの分は捨てる。
  const running = useRef<Record<string, object>>({})

  // コマンドが終わった会話の、実行中にだけ出す表示を外す。
  const clearRunning = useCallback((key: string) => {
    setPending((prev) => without(prev, key))
    setLive((prev) => without(prev, key))
    setStopping((prev) => without(prev, key))
  }, [])

  const fail = (key: string, e: unknown) => {
    setFailures((prev) => ({
      ...prev,
      [key]: t('chat.command_failed', { error: isolated(failureText(e)) }),
    }))
  }

  const reloaded = useCallback(
    (chat: Chat) => {
      const key = chatKey(chat)
      if (running.current[key] === undefined) {
        clearRunning(key)
        return
      }
      setPending((prev) =>
        prev[key] === undefined
          ? prev
          : { ...prev, [key]: prev[key].filter((entry) => entry.role !== 'user') },
      )
    },
    [clearRunning],
  )

  const run: ChatRequests['run'] = async (chat, entries, command, settle) => {
    const key = chatKey(chat)
    const token = {}
    running.current[key] = token
    setPending((prev) => ({ ...prev, [key]: entries }))
    setStopping((prev) => without(prev, key))
    setFailures((prev) => without(prev, key))
    const onEvent = (event: TurnEvent) => {
      if (running.current[key] !== token) return
      setLive((prev) => ({
        ...prev,
        [key]: appendTurnEvent(prev[key] ?? NO_LIVE_TURN, event),
      }))
    }
    try {
      await command(onEvent)
    } catch (e) {
      fail(key, e)
    }
    // 引き直しの最中に届いた分を積まないよう、引き直す前に外す。
    running.current = without(running.current, key)
    await settle(chat)
    // 表示中の会話は`reloaded`で外れて操作できるようになっている。その間に同じ会話で次の
    // コマンドが始まっていたら、そちらの楽観表示と途中経過を消さない。
    if (running.current[key] !== undefined) return
    clearRunning(key)
  }

  const stop: ChatRequests['stop'] = async (chat, command) => {
    const key = chatKey(chat)
    const token = running.current[key]
    if (token === undefined) return
    setStopping((prev) => ({ ...prev, [key]: true }))
    let stopped = false
    try {
      stopped = await command()
    } catch (e) {
      fail(key, e)
    }
    // 待っている間にコマンドが終わって次が始まっていたら、そちらの表示には触れない。
    if (!stopped && running.current[key] === token) {
      setStopping((prev) => without(prev, key))
    }
  }

  const isStopping = (chat: Chat) => stopping[chatKey(chat)] !== undefined

  return {
    pendingOf: (chat) => {
      const entries = pending[chatKey(chat)] ?? []
      if (!isStopping(chat)) return entries
      return entries.map((entry) =>
        entry.role === 'pending'
          ? { ...entry, content: t('chat.stopping_reply'), stopping: true }
          : entry,
      )
    },
    liveOf: (chat) => live[chatKey(chat)]?.segments ?? NO_SEGMENTS,
    isBusy: (chat) => pending[chatKey(chat)] !== undefined,
    isGenerating: (chat) =>
      pending[chatKey(chat)]?.some((entry) => entry.role === 'pending') ?? false,
    isStopping,
    failureOf: (chat) => failures[chatKey(chat)] ?? null,
    reloaded,
    run,
    stop,
  }
}
