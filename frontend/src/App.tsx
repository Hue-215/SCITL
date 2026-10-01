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
  stopChatResponse,
} from './api'
import { StagedAttachmentChips } from './Attachments'
import ChatLog from './ChatLog'
import { chatKey, GENERAL_CHAT, taskChat } from './chat'
import ChatModelBar from './ChatModelBar'
import { t, turnErrorText } from './i18n'
import Settings from './Settings'
import Sidebar from './Sidebar'
import TaskHeader from './TaskHeader'
import type {
  AttachmentDeliveries,
  Chat,
  MessageView,
  SelectedModel,
  TaskDetailView,
  TaskListItem,
} from './types'
import { useChatRequests } from './useChatRequests'
import { useStagedAttachments } from './useStagedAttachments'
import { useStickToBottom } from './useStickToBottom'
import { isCommitEnter } from './keyboard'

export default function App() {
  const [tasks, setTasks] = useState<TaskListItem[]>([])
  // 表示中の会話。起動したら総合チャットを開く(タスクを離れたときの戻り先でもある)。
  const [chat, setChat] = useState<Chat>(GENERAL_CHAT)
  const [adding, setAdding] = useState(false)
  // タスクを作らなかった理由(チャットを使えない間)。IPCの失敗(`error`)と違い、モデルの
  // 選択や設定の変更で解消しうるので、それらを変えたら外す(次の追加で改めて判定される)。
  const [addBlocked, setAddBlocked] = useState<string | null>(null)
  // 表示中のタスク。総合チャットと、タスクを読み込むまでの間はnull。
  const [task, setTask] = useState<TaskDetailView | null>(null)
  const [messages, setMessages] = useState<MessageView[]>([])
  const [draft, setDraft] = useState('')
  // 入力欄の送信前の添付。本文と同じく、会話を切り替えても残す。
  const staged = useStagedAttachments()
  const fileInputRef = useRef<HTMLInputElement>(null)
  // 選んでいるモデルが添付を種別ごとにどう受け取るか。警告の判断はRust側が済ませてある。
  const [deliveries, setDeliveries] = useState<AttachmentDeliveries | null>(null)
  const onModelSelected = useCallback(
    (selected: SelectedModel | null) => setDeliveries(selected?.attachments ?? null),
    [],
  )
  // 会話に属さない操作(一覧・作成・読み込み)の失敗。会話へのコマンドの失敗は
  // `requests`が会話ごとに持つ。
  const [error, setError] = useState<string | null>(null)
  const [settingsOpen, setSettingsOpen] = useState(false)
  // 編集モード。ユーザー発言のみが対象。応答待ち中は開始できない(`disableActions`参照)。
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

  // 作ったらユーザーの発言を待たずに聞き取りを始める。
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

  // 応答待ちの会話では、送信・編集・再試行・削除のすべてを不可にする。他の会話は応答待ちの
  // 間も操作できる。
  const disableActions = requests.isBusy(chat)
  // 応答を生成中の会話では、送信ボタンの位置に停止ボタンを出す。
  const generating = requests.isGenerating(chat)

  // 本文が空でも、添付があれば送れる。判定を待っている添付があるうちは送らない。
  const canSend = !disableActions && !staged.busy && (draft.trim() !== '' || staged.ready)

  const send = async () => {
    if (!canSend) return
    const text = draft.trim()
    const target = chat
    const attachments = staged.take()
    setDraft('')
    stick()
    // 楽観表示はユーザー発言と応答待ちプレースホルダのみに留め、応答本体は確定後に
    // DBから引き直す。
    await requests.run(
      target,
      [
        { role: 'user', content: text, attachmentNames: attachments.names },
        { role: 'pending', content: t('chat.pending_reply') },
      ],
      (onEvent) =>
        sendChatMessage(target, text, attachments.tokens, onEvent).catch((e: unknown) => {
          staged.restore(attachments)
          throw e
        }),
      settle,
    )
  }

  // 編集・再試行で置き換わる行を、応答の確定を待たずに画面から外す。バックエンドは応答の生成に
  // 入る前に論理削除を済ませているので、起きた削除を先に見せるだけになる(確定後は`loadChat`が
  // DBの内容で上書きする)。
  //
  // `turnId`は再試行でのみ渡す。idだけで切ると同じターンのツール実行記録が残り、
  // `finalEntryOf`がそれを返信の吹き出しとして描いてしまうので、ターンごと外す。
  const hideSuperseded = (fromId: number, turnId: string | null) => {
    setMessages((prev) =>
      prev.filter((m) => m.id < fromId && (turnId === null || m.turn_id !== turnId)),
    )
  }

  // 添付は新しい発言へ引き継がれるので、添付のある発言は本文を空にしても送れる。
  const submitEdit = async (message: MessageView) => {
    const messageId = message.id
    const text = editDraft.trim()
    if ((!text && message.attachments.length === 0) || disableActions) return
    const target = chat
    setEditingId(null)
    stick()
    hideSuperseded(messageId, null)
    await requests.run(
      target,
      [
        {
          role: 'user',
          content: text,
          attachmentNames: message.attachments.map((a) => a.original_name),
        },
        { role: 'pending', content: t('chat.pending_reply') },
      ],
      (onEvent) => editChatMessage(target, messageId, text, onEvent),
      settle,
    )
  }

  const stop = () => {
    const target = chat
    void requests.stop(target, () => stopChatResponse(target))
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

  // 発言とそれより後ろをまとめて削除する(確認は発言の操作ボタンが挟む)。最初のユーザー発言を
  // 消すと一覧のフォールバック表示が変わるので、`requests`が一覧ごと引き直す。
  const remove = async (messageId: number) => {
    if (disableActions) return
    const target = chat
    await requests.run(target, [], () => deleteChatMessage(target, messageId), settle)
  }

  // ヘッダーからのタスク操作。発言の操作と同じく会話ごとの応答待ちに載せ、
  // 実行中は他の操作を止め、失敗はその会話に残す。アーカイブ・削除のあとは総合チャットへ
  // 戻る。その間に別の会話へ移っていたら、そのままにする。
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

        <ChatLog
          logRef={logRef}
          onScroll={onLogScroll}
          messages={messages}
          pending={pending}
          live={live}
          failure={failure}
          disableActions={disableActions}
          editing={{
            id: editingId,
            draft: editDraft,
            setDraft: setEditDraft,
            start: (message) => {
              setEditingId(message.id)
              setEditDraft(message.content)
            },
            cancel: () => setEditingId(null),
            submit: (message) => void submitEdit(message),
          }}
          onRetry={(messageId) => void retry(messageId)}
          onRemove={(messageId) => void remove(messageId)}
        />

        <StagedAttachmentChips staged={staged} deliveries={deliveries} disabled={disableActions} />

        <div className="chat-compose-area">
          <form
            className="chat-compose"
            onSubmit={(e) => {
              e.preventDefault()
              void send()
            }}
          >
            <input
              ref={fileInputRef}
              type="file"
              multiple
              hidden
              onChange={(e) => {
                staged.add(Array.from(e.target.files ?? []))
                // 同じファイルをもう一度選んでも変更として届くように空へ戻す。
                e.target.value = ''
              }}
            />
            <button
              type="button"
              disabled={disableActions || !staged.canAdd}
              title={t('attachment.add_tooltip')}
              onClick={() => fileInputRef.current?.click()}
            >
              {t('attachment.add_button')}
            </button>
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
            {generating ? (
              // 送信ボタンとは別の要素にする(同じ要素だと、送信を押したフォーカスが残り、
              // 応答待ちの間のEnterで止めてしまう)。
              <button
                key="stop"
                type="button"
                className="primary"
                disabled={requests.isStopping(chat)}
                onClick={(e) => {
                  // 送信をダブルクリックした2回目が、入れ替わった停止ボタンに当たっても止めない。
                  if (e.detail > 1) return
                  stop()
                }}
              >
                {t('chat.stop_button')}
              </button>
            ) : (
              <button key="send" type="submit" className="primary" disabled={!canSend}>
                {t('chat.send_button')}
              </button>
            )}
          </form>

          <ChatModelBar
            onError={setError}
            onChanged={() => setAddBlocked(null)}
            onSelected={onModelSelected}
          />
        </div>
      </main>
    </div>
  )
}
