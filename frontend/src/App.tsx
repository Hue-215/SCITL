import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react'
import {
  createTask,
  deleteTaskChatMessage,
  editTaskChatMessage,
  getTaskDetail,
  listTaskMessages,
  listTasks,
  retryTaskChatMessage,
  sendTaskChatMessage,
} from './api'
import Markdown from './Markdown'
import Settings from './Settings'
import Sidebar from './Sidebar'
import { taskName } from './taskName'
import { ExternalToolLine, ThinkingTools } from './ThinkingTools'
import { finalEntryOf, groupMessages } from './thinking'
import type { Message, TaskDetail, TaskSummary } from './types'
import { useStickToBottom } from './useStickToBottom'
import { useTaskRequests } from './useTaskRequests'
import { isCommitEnter } from './keyboard'

function formatTime(createdAt: string): string {
  return new Date(createdAt).toLocaleString()
}

// Markdownとして描画するのはユーザーとモデルが書いた本文だけ。エラー発言と応答待ちの
// 表示はSCITL自身の文言(とプロバイダーが返した文字列)なので、プレーンテキストのまま出す。
function EntryBody({ role, content }: { role: string; content: string }) {
  if (role === 'user' || role === 'assistant') return <Markdown text={content} />
  return <span className="entry-content">{content}</span>
}

export default function App() {
  const [tasks, setTasks] = useState<TaskSummary[]>([])
  const [taskId, setTaskId] = useState<number | null>(null)
  const [adding, setAdding] = useState(false)
  const [task, setTask] = useState<TaskDetail | null>(null)
  const [messages, setMessages] = useState<Message[]>([])
  const [draft, setDraft] = useState('')
  // タスクに属さない操作(一覧・作成・読み込み)の失敗。タスクへのコマンドの失敗は
  // `requests`がタスクごとに持つ。
  const [error, setError] = useState<string | null>(null)
  const [settingsOpen, setSettingsOpen] = useState(false)
  // 編集モード(Issue #41)。ユーザー発言のみが対象。応答待ち中は開始できない
  // (`disableActions`参照)。
  const [editingId, setEditingId] = useState<number | null>(null)
  const [editDraft, setEditDraft] = useState('')
  // 選択中のタスク。非同期の処理が終わった時点で見比べるため、stateとは別にrefでも持つ
  // (処理を始めたときのstateは古いままなので、比べても切り替えに気付けない)。
  const selectedRef = useRef<number | null>(null)
  const {
    ref: logRef,
    onScroll: onLogScroll,
    stick,
    follow,
  } = useStickToBottom<HTMLUListElement>()

  const loadTasks = useCallback(async () => {
    try {
      const summaries = await listTasks()
      setTasks(summaries)
      setError(null)
      return summaries
    } catch (e) {
      setError(String(e))
      return []
    }
  }, [])

  const selectTask = useCallback((id: number) => {
    if (selectedRef.current === id) return
    selectedRef.current = id
    stick()
    setTaskId(id)
    // 読み込みが終わるまで前のタスクの内容を出しておくと、それを見ながら新しいタスクへ
    // 操作できてしまう。
    setTask(null)
    setMessages([])
  }, [stick])

  useEffect(() => {
    void loadTasks().then((summaries) => {
      if (summaries.length > 0) selectTask(summaries[0].id)
    })
  }, [loadTasks, selectTask])

  const requests = useTaskRequests()
  const { reloaded } = requests

  const loadTask = useCallback(
    async (id: number) => {
      try {
        const [detail, history] = await Promise.all([getTaskDetail(id), listTaskMessages(id)])
        // 読み込み中に別のタスクへ移っていたら捨てる。追い越した結果で表示を上書きしない。
        if (selectedRef.current !== id) return
        setTask(detail)
        setMessages(history)
        setEditingId(null)
        reloaded(id)
      } catch (e) {
        if (selectedRef.current === id) setError(String(e))
      }
    },
    [reloaded],
  )

  // コマンドが終わったら、そのタスクを見ているときだけ引き直す。一覧は常に引き直す
  // (タイトル・工程の進捗が変わりうるため)。
  const settle = async (id: number) => {
    if (selectedRef.current === id) await loadTask(id)
    await loadTasks()
  }

  useEffect(() => {
    if (taskId !== null) void loadTask(taskId)
  }, [taskId, loadTask])

  const addTask = async () => {
    if (adding) return
    setAdding(true)
    try {
      const created = await createTask()
      await loadTasks()
      selectTask(created.id)
      setError(null)
    } catch (e) {
      setError(String(e))
    } finally {
      setAdding(false)
    }
  }

  // 応答待ちのタスクでは、送信・編集・再試行・削除のすべてを不可にする(Issue #41、
  // legacy/frontend.md 1節)。他のタスクは応答待ちの間も操作できる。
  const disableActions = taskId === null || requests.isBusy(taskId)

  const send = async () => {
    const text = draft.trim()
    if (!text || disableActions || taskId === null) return
    const id = taskId
    setDraft('')
    stick()
    // 楽観表示はユーザー発言と応答待ちプレースホルダのみに留め、応答本体は確定後に
    // DBから引き直す(docs/spec/principles.md 3節「保存するのは組み立て終わった応答」)。
    await requests.run(
      id,
      [
        { role: 'user', content: text },
        { role: 'pending', content: '応答待ち…' },
      ],
      () => sendTaskChatMessage(id, text),
      settle,
    )
  }

  // 編集・再試行で置き換わる行を、応答の確定を待たずに画面から外す(Issue #95)。
  // バックエンドはコマンド最初のトランザクションで論理削除まで済ませてから応答生成に入るので、
  // ここでやっているのは「すでに起きた削除を先に見せる」ことだけ。確定後は`loadTask`が必ず
  // DBの内容で上書きするため、これが最終的な表示になることはない(楽観表示はユーザー発言の
  // プレースホルダと同じ扱い)。
  //
  // `turnId`は再試行でのみ渡す。再試行の対象はターンの最終行なので、idだけで切ると同じターンの
  // ツール実行記録が残り、`finalEntryOf`がそれを返信の吹き出しとして描いてしまう
  // (`thinking.ts`参照)。作り直すのはターンごとなので、ターンごと外す。
  const hideSuperseded = (fromId: number, turnId: string | null) => {
    setMessages((prev) =>
      prev.filter((m) => m.id < fromId && (turnId === null || m.turn_id !== turnId)),
    )
  }

  const submitEdit = async (messageId: number) => {
    const text = editDraft.trim()
    if (!text || disableActions || taskId === null) return
    const id = taskId
    setEditingId(null)
    stick()
    hideSuperseded(messageId, null)
    await requests.run(
      id,
      [
        { role: 'user', content: text },
        { role: 'pending', content: '応答待ち…' },
      ],
      () => editTaskChatMessage(id, messageId, text),
      settle,
    )
  }

  const retry = async (messageId: number) => {
    if (disableActions || taskId === null) return
    const id = taskId
    stick()
    hideSuperseded(messageId, messages.find((m) => m.id === messageId)?.turn_id ?? null)
    await requests.run(
      id,
      [{ role: 'pending', content: '応答待ち…' }],
      () => retryTaskChatMessage(id, messageId),
      settle,
    )
  }

  // 確認ダイアログ無しの即座に取り消し可能な論理削除(legacy/frontend.md 1節)。最初の
  // ユーザー発言を消すと一覧のフォールバック表示が変わる(Issue #61)が、引き直しは
  // `requests`が一覧ごと行う。
  const remove = async (messageId: number) => {
    if (disableActions || taskId === null) return
    const id = taskId
    await requests.run(id, [], () => deleteTaskChatMessage(id, messageId), settle)
  }

  const pending = taskId === null ? [] : requests.pendingOf(taskId)
  const failure = taskId === null ? null : requests.failureOf(taskId)

  // 会話欄の中身が変わるのは、発言の引き直し・楽観表示の出し入れ・失敗の表示のとき。
  // 設定画面から戻ったときは会話欄が作り直されて先頭に戻るので、それも含める。
  useLayoutEffect(follow, [follow, messages, pending.length, failure, settingsOpen])

  if (settingsOpen) {
    return (
      <Settings
        onClose={() => {
          stick()
          setSettingsOpen(false)
        }}
      />
    )
  }

  return (
    <div className="layout">
      <Sidebar
        tasks={tasks}
        selectedTaskId={taskId}
        onSelect={selectTask}
        onAddTask={() => void addTask()}
        adding={adding}
        onOpenSettings={() => setSettingsOpen(true)}
      />

      <main>
        <header className="chat-header">
          <h1>{task ? taskName(task) : 'SCITL'}</h1>
          {task?.description && <p>{task.description}</p>}
        </header>

        {error && <p className="error">{error}</p>}

        <ul className="chat-log" ref={logRef} onScroll={onLogScroll}>
          {groupMessages(messages).map((item) => {
            if (item.kind === 'plain') {
              const message = item.message
              // 外部(MCP)経由のツール呼び出しは「思考・ツール」の折りたたみに含めず、
              // 独立した1行として表示する(docs/spec/legacy/frontend.md 1節)。
              if (message.kind === 'tool_execution') {
                return (
                  <li key={message.id} className="entry entry-tool">
                    <ExternalToolLine message={message} />
                    <time className="entry-time">{formatTime(message.created_at)}</time>
                  </li>
                )
              }

              // 編集・削除(Issue #41)。対象はツール実行記録を除く通常発言のみ
              // (data-model.md「ツール実行記録は通常発言の編集・削除・再試行の対象に
              // 含めない」)。`plain`項目は常にユーザー発言のため、編集はここでのみ
              // 起こりうる(legacy/frontend.md 1節)。編集と削除は対象が同じ。
              const canEditOrDelete = message.role === 'user'

              if (editingId === message.id) {
                return (
                  <li key={message.id} className={`entry entry-${message.role}`}>
                    <textarea
                      className="entry-edit-textarea"
                      value={editDraft}
                      onChange={(e) => setEditDraft(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
                          e.preventDefault()
                          void submitEdit(message.id)
                        } else if (e.key === 'Escape') {
                          setEditingId(null)
                        }
                      }}
                      autoFocus
                    />
                    <div className="entry-actions">
                      <button type="button" onClick={() => setEditingId(null)}>
                        キャンセル
                      </button>
                      <button type="button" onClick={() => void submitEdit(message.id)}>
                        送信
                      </button>
                    </div>
                  </li>
                )
              }

              return (
                <li key={message.id} className={`entry entry-${message.role}`}>
                  <EntryBody role={message.role} content={message.content} />
                  <time className="entry-time">{formatTime(message.created_at)}</time>
                  {canEditOrDelete && (
                    <div className="entry-actions">
                      <button
                        type="button"
                        disabled={disableActions}
                        onClick={() => {
                          setEditingId(message.id)
                          setEditDraft(message.content)
                        }}
                      >
                        編集
                      </button>
                      <button
                        type="button"
                        disabled={disableActions}
                        onClick={() => void remove(message.id)}
                      >
                        削除
                      </button>
                    </div>
                  )}
                </li>
              )
            }

            // SCITL自身の応答生成1ターン分。思考・内部ツール呼び出しを発生順の折りたたみで
            // 見せたうえで、実際の返信(最終行)を通常の吹き出しとして表示する(Issue #42)。
            // 再試行・削除(Issue #41)の対象は、この最終行の通常発言のみ
            // (data-model.md「ツール実行記録は…対象に含めない」)。再試行と削除は対象が同じ。
            // 失敗したターンの返信(エラー発言)も含める(Issue #130)。
            const finalMessage = finalEntryOf(item.entries)
            const canRetryOrDelete =
              finalMessage.kind === 'normal' &&
              (finalMessage.role === 'assistant' || finalMessage.role === 'error')
            return (
              <li key={`turn-${item.turnId}`} className="turn-group">
                <ThinkingTools entries={item.entries} />
                <div className={`entry entry-${finalMessage.role}`}>
                  <EntryBody role={finalMessage.role} content={finalMessage.content} />
                  {/* プロバイダーが書いた文字列のため、Markdown描画(#39)の対象にせず
                      プレーンテキストのまま出す(Issue #159) */}
                  {finalMessage.error_detail && (
                    <details className="entry-error-detail">
                      <summary>詳細を表示</summary>
                      <pre>{finalMessage.error_detail}</pre>
                    </details>
                  )}
                  <time className="entry-time">{formatTime(finalMessage.created_at)}</time>
                  {canRetryOrDelete && (
                    <div className="entry-actions">
                      <button
                        type="button"
                        disabled={disableActions}
                        onClick={() => void retry(finalMessage.id)}
                      >
                        再試行
                      </button>
                      <button
                        type="button"
                        disabled={disableActions}
                        onClick={() => void remove(finalMessage.id)}
                      >
                        削除
                      </button>
                    </div>
                  )}
                </div>
              </li>
            )
          })}
          {pending.map((entry, i) => (
            <li key={`pending-${i}`} className={`entry entry-${entry.role}`}>
              <EntryBody role={entry.role} content={entry.content} />
            </li>
          ))}
          {/* コマンド自体の失敗。保存されたエラー発言と同じ見た目にする(Issue #152) */}
          {failure && (
            <li className="entry entry-error">
              <span className="entry-content">{failure}</span>
            </li>
          )}
        </ul>

        <form
          className="chat-compose"
          onSubmit={(e) => {
            e.preventDefault()
            void send()
          }}
        >
          <textarea
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (isCommitEnter(e) && !e.shiftKey) {
                e.preventDefault()
                void send()
              }
            }}
            disabled={disableActions}
            placeholder="タスクについて話しかける"
          />
          <button type="submit" disabled={disableActions || !draft.trim()}>
            送信
          </button>
        </form>
      </main>
    </div>
  )
}
