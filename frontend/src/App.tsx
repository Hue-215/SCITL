import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react'
import {
  createTask,
  deleteChatMessage,
  deleteTask,
  editChatMessage,
  failureText,
  getTaskDetail,
  listChatMessages,
  listTasks,
  openTaskChat,
  renameTask,
  retryChatMessage,
  sendChatMessage,
  setTaskArchived,
} from './api'
import { chatKey, GENERAL_CHAT, taskChat } from './chat'
import ChatModelBar from './ChatModelBar'
import { formatDateTime, t, turnErrorText } from './i18n'
import Markdown from './Markdown'
import Settings from './Settings'
import Sidebar from './Sidebar'
import TaskHeader from './TaskHeader'
import { OperationLine, ThinkingTools } from './ThinkingTools'
import { buildThoughtItems, finalEntryOf, groupMessages } from './thinking'
import type { Chat, Message, TaskDetail, TaskSummary } from './types'
import { useChatRequests } from './useChatRequests'
import { useStickToBottom } from './useStickToBottom'
import { isCommitEnter } from './keyboard'

// Markdownとして描画するのはユーザーとモデルが書いた本文だけ。エラー発言と応答待ちの
// 表示はSCITL自身の文言(とプロバイダーが返した文字列)なので、プレーンテキストのまま出す。
// エラー発言は、ターンの中でも外でもここで表示言語の文言に替える。
function EntryBody({
  role,
  content,
  errorKind = null,
}: {
  role: string
  content: string
  errorKind?: string | null
}) {
  if (role === 'user' || role === 'assistant') return <Markdown text={content} />
  const text = role === 'error' ? turnErrorText(errorKind, content) : content
  return <span className="entry-content">{text}</span>
}

export default function App() {
  const [tasks, setTasks] = useState<TaskSummary[]>([])
  // 表示中の会話。起動したら総合チャットを開く(タスクを離れたときの戻り先でもある)。
  const [chat, setChat] = useState<Chat>(GENERAL_CHAT)
  const [adding, setAdding] = useState(false)
  // タスクを作らなかった理由(チャットを使えない間。Issue #76)。IPCの失敗(`error`)と違い、
  // モデルの選択や設定の変更で解消しうるので、それらを変えたら外す(次の追加で改めて判定される)。
  const [addBlocked, setAddBlocked] = useState<string | null>(null)
  // 表示中のタスク。総合チャットと、タスクを読み込むまでの間はnull。
  const [task, setTask] = useState<TaskDetail | null>(null)
  const [messages, setMessages] = useState<Message[]>([])
  const [draft, setDraft] = useState('')
  // 会話に属さない操作(一覧・作成・読み込み)の失敗。会話へのコマンドの失敗は
  // `requests`が会話ごとに持つ。
  const [error, setError] = useState<string | null>(null)
  const [settingsOpen, setSettingsOpen] = useState(false)
  // 編集モード(Issue #41)。ユーザー発言のみが対象。応答待ち中は開始できない
  // (`disableActions`参照)。
  const [editingId, setEditingId] = useState<number | null>(null)
  const [editDraft, setEditDraft] = useState('')
  // 表示中の会話の鍵(`chatKey`)。非同期の処理が終わった時点で見比べるため、stateとは別に
  // refでも持つ(処理を始めたときのstateは古いままなので、比べても切り替えに気付けない)。
  const selectedRef = useRef(chatKey(GENERAL_CHAT))
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
      setError(failureText(e))
      return []
    }
  }, [])

  const selectChat = useCallback(
    (next: Chat) => {
      const key = chatKey(next)
      if (selectedRef.current === key) return
      selectedRef.current = key
      stick()
      setChat(next)
      // 読み込みが終わるまで前の会話の内容を出しておくと、それを見ながら新しい会話へ
      // 操作できてしまう。
      setTask(null)
      setMessages([])
    },
    [stick],
  )

  useEffect(() => {
    void loadTasks()
  }, [loadTasks])

  const requests = useChatRequests()
  const { reloaded } = requests

  const loadChat = useCallback(
    async (target: Chat) => {
      const key = chatKey(target)
      try {
        const [detail, history] = await Promise.all([
          target.kind === 'task' ? getTaskDetail(target.task_id) : null,
          listChatMessages(target),
        ])
        // 読み込み中に別の会話へ移っていたら捨てる。追い越した結果で表示を上書きしない。
        if (selectedRef.current !== key) return
        setTask(detail)
        setMessages(history)
        setEditingId(null)
        reloaded(target)
      } catch (e) {
        if (selectedRef.current === key) setError(failureText(e))
      }
    },
    [reloaded],
  )

  // コマンドが終わったら、その会話を見ているときだけ引き直す。一覧は常に引き直す
  // (タイトル・工程の進捗が変わりうるため)。
  const settle = async (target: Chat) => {
    if (selectedRef.current === chatKey(target)) await loadChat(target)
    await loadTasks()
  }

  useEffect(() => {
    void loadChat(chat)
  }, [chat, loadChat])

  // 作ったらユーザーの発言を待たずに聞き取りを始める(Issue #76、legacy/frontend.md 1節)。
  const addTask = async () => {
    if (adding) return
    setAdding(true)
    setAddBlocked(null)
    let id: number
    try {
      const result = await createTask()
      if (result.status === 'unavailable') {
        // モデル未選択等でチャットを使えない間は作らない。理由はエラー発言と同じ文言で出す。
        setAddBlocked(turnErrorText(result.error_kind, result.error_kind))
        return
      }
      id = result.task.id
      await loadTasks()
      selectChat(taskChat(id))
    } catch (e) {
      setError(failureText(e))
      return
    } finally {
      setAdding(false)
    }
    await requests.run(
      taskChat(id),
      [{ role: 'pending', content: t('chat.pending_reply') }],
      (onEvent) => openTaskChat(id, onEvent),
      settle,
    )
  }

  // 応答待ちの会話では、送信・編集・再試行・削除のすべてを不可にする(Issue #41、
  // legacy/frontend.md 1節)。他の会話は応答待ちの間も操作できる。
  const disableActions = requests.isBusy(chat)

  const send = async () => {
    const text = draft.trim()
    if (!text || disableActions) return
    const target = chat
    setDraft('')
    stick()
    // 楽観表示はユーザー発言と応答待ちプレースホルダのみに留め、応答本体は確定後に
    // DBから引き直す(docs/spec/principles.md 3節「保存するのは組み立て終わった応答」)。
    await requests.run(
      target,
      [
        { role: 'user', content: text },
        { role: 'pending', content: t('chat.pending_reply') },
      ],
      (onEvent) => sendChatMessage(target, text, onEvent),
      settle,
    )
  }

  // 編集・再試行で置き換わる行を、応答の確定を待たずに画面から外す(Issue #95)。
  // バックエンドはコマンド最初のトランザクションで論理削除まで済ませてから応答生成に入るので、
  // ここでやっているのは「すでに起きた削除を先に見せる」ことだけ。確定後は`loadChat`が必ず
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
    if (!text || disableActions) return
    const target = chat
    setEditingId(null)
    stick()
    hideSuperseded(messageId, null)
    await requests.run(
      target,
      [
        { role: 'user', content: text },
        { role: 'pending', content: t('chat.pending_reply') },
      ],
      (onEvent) => editChatMessage(target, messageId, text, onEvent),
      settle,
    )
  }

  const retry = async (messageId: number) => {
    if (disableActions) return
    const target = chat
    stick()
    hideSuperseded(messageId, messages.find((m) => m.id === messageId)?.turn_id ?? null)
    await requests.run(
      target,
      [{ role: 'pending', content: t('chat.pending_reply') }],
      (onEvent) => retryChatMessage(target, messageId, onEvent),
      settle,
    )
  }

  // 確認ダイアログ無しの即座に取り消し可能な論理削除(legacy/frontend.md 1節)。最初の
  // ユーザー発言を消すと一覧のフォールバック表示が変わる(Issue #61)が、引き直しは
  // `requests`が一覧ごと行う。
  const remove = async (messageId: number) => {
    if (disableActions) return
    const target = chat
    await requests.run(target, [], () => deleteChatMessage(target, messageId), settle)
  }

  // ヘッダーからのタスク操作(Issue #75)。発言の操作と同じく会話ごとの応答待ちに載せ、
  // 実行中は他の操作を止め、失敗はその会話に残す。アーカイブ・削除のあとは総合チャットへ
  // 戻る(legacy/frontend.md 1節)。その間に別の会話へ移っていたら、そのままにする。
  const runTaskOperation = (taskId: number, operation: () => Promise<unknown>) => {
    if (disableActions) return
    const target = taskChat(taskId)
    void requests.run(target, [], operation, settle)
  }
  const leaveIfShown = (taskId: number) => {
    if (selectedRef.current === chatKey(taskChat(taskId))) selectChat(GENERAL_CHAT)
  }

  const pending = requests.pendingOf(chat)
  const live = requests.liveOf(chat)
  const failure = requests.failureOf(chat)

  // 会話欄の中身が変わるのは、発言の引き直し・楽観表示の出し入れ・途中経過の到着・失敗の
  // 表示のとき。設定画面から戻ったときは会話欄が作り直されて先頭に戻るので、それも含める。
  useLayoutEffect(follow, [follow, messages, pending.length, live.length, failure, settingsOpen])

  if (settingsOpen) {
    return (
      <Settings
        onClose={() => {
          stick()
          setAddBlocked(null)
          setSettingsOpen(false)
        }}
      />
    )
  }

  return (
    <div className="layout">
      <Sidebar
        tasks={tasks}
        selected={chat}
        onSelect={selectChat}
        onAddTask={() => void addTask()}
        adding={adding}
        onOpenSettings={() => setSettingsOpen(true)}
      />

      <main>
        {task ? (
          <TaskHeader
            key={task.id}
            task={task}
            disabled={disableActions}
            onRename={(title) => runTaskOperation(task.id, () => renameTask(task.id, title))}
            onSetArchived={(archived) =>
              runTaskOperation(task.id, async () => {
                await setTaskArchived(task.id, archived)
                if (archived) leaveIfShown(task.id)
              })
            }
            onDelete={() =>
              runTaskOperation(task.id, async () => {
                await deleteTask(task.id)
                leaveIfShown(task.id)
              })
            }
          />
        ) : (
          <header className="chat-header">
            <h1>{chat.kind === 'general' ? t('chat.general_title') : t('common.app_name')}</h1>
          </header>
        )}

        {error && <p className="error">{error}</p>}
        {addBlocked && <p className="error">{addBlocked}</p>}

        <ul className="chat-log" ref={logRef} onScroll={onLogScroll}>
          {groupMessages(messages).map((item) => {
            if (item.kind === 'plain') {
              const message = item.message
              // 応答生成以外の経路(画面・MCP等)での操作の記録は「思考・ツール」の
              // 折りたたみに含めず、独立した1行として表示する(docs/spec/legacy/frontend.md 1節)。
              if (message.kind === 'tool_execution') {
                return (
                  <li key={message.id} className="entry entry-tool">
                    <OperationLine message={message} />
                    <time className="entry-time">{formatDateTime(message.created_at)}</time>
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
                        {t('common.cancel')}
                      </button>
                      <button type="button" onClick={() => void submitEdit(message.id)}>
                        {t('chat.send_button')}
                      </button>
                    </div>
                  </li>
                )
              }

              return (
                <li key={message.id} className={`entry entry-${message.role}`}>
                  <EntryBody
                    role={message.role}
                    content={message.content}
                    errorKind={message.error_kind}
                  />
                  <time className="entry-time">{formatDateTime(message.created_at)}</time>
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
                        {t('chat.edit_button')}
                      </button>
                      <button
                        type="button"
                        disabled={disableActions}
                        onClick={() => void remove(message.id)}
                      >
                        {t('common.delete')}
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
                <ThinkingTools items={buildThoughtItems(item.entries)} />
                <div className={`entry entry-${finalMessage.role}`}>
                  <EntryBody
                    role={finalMessage.role}
                    content={finalMessage.content}
                    errorKind={finalMessage.error_kind}
                  />
                  {/* プロバイダーが書いた文字列のため、Markdown描画(#39)の対象にせず
                      プレーンテキストのまま出す(Issue #159) */}
                  {finalMessage.error_detail && (
                    <details className="entry-error-detail">
                      <summary>{t('chat.error_detail_summary')}</summary>
                      <pre>{finalMessage.error_detail}</pre>
                    </details>
                  )}
                  <time className="entry-time">{formatDateTime(finalMessage.created_at)}</time>
                  {canRetryOrDelete && (
                    <div className="entry-actions">
                      <button
                        type="button"
                        disabled={disableActions}
                        onClick={() => void retry(finalMessage.id)}
                      >
                        {t('chat.retry_button')}
                      </button>
                      <button
                        type="button"
                        disabled={disableActions}
                        onClick={() => void remove(finalMessage.id)}
                      >
                        {t('common.delete')}
                      </button>
                    </div>
                  )}
                </div>
              </li>
            )
          })}
          {pending.map((entry, i) =>
            entry.role === 'pending' ? (
              // 応答待ちの間の途中経過を、保存済みのターンと同じ形で出す(Issue #70)。
              // 完了したら読み直したターンに置き換わる。
              <li key={`pending-${i}`} className="turn-group">
                <ThinkingTools items={live} />
                <div className="entry entry-pending">
                  <EntryBody role={entry.role} content={entry.content} />
                </div>
              </li>
            ) : (
              <li key={`pending-${i}`} className={`entry entry-${entry.role}`}>
                <EntryBody role={entry.role} content={entry.content} />
              </li>
            ),
          )}
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
            placeholder={t('chat.input_hint')}
          />
          <button type="submit" disabled={disableActions || !draft.trim()}>
            {t('chat.send_button')}
          </button>
        </form>

        <ChatModelBar onError={setError} onChanged={() => setAddBlocked(null)} />
      </main>
    </div>
  )
}
