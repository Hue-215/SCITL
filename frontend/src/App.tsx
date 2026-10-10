import { useCallback, useEffect, useEffectEvent, useLayoutEffect, useRef, useState } from 'react'
import {
  chatLacksReply,
  createTask,
  deleteChatMessage,
  deleteTask,
  editChatMessage,
  failureText,
  generateChatReply,
  getTaskDetail,
  listChatMessages,
  listTasks,
  renameTask,
  retryChatMessage,
  sendChatMessage,
  setTaskArchived,
  stopChatResponse,
  watchRequestedChats,
} from './api'
import ChatCompose, { type ComposedMessage } from './ChatCompose'
import ChatLog from './ChatLog'
import { chatKey, GENERAL_CHAT, taskChat } from './chat'
import { Drawer, DrawerToggle } from './Drawer'
import { t, turnErrorText } from './i18n'
import Settings from './Settings'
import Sidebar from './Sidebar'
import TaskHeader from './TaskHeader'
import { stoppedTurnAtEnd } from './thinking'
import type { Chat, MessageView, Task, TaskDetailView, TaskListItem, TurnEvent } from './types'
import { useChatRequests } from './useChatRequests'
import { useCloseOnBack } from './useCloseOnBack'
import { useDrawer } from './useDrawer'
import { useSwipeToOpen } from './useDrawerSwipe'
import { useStickToBottom } from './useStickToBottom'

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
  // 表示中の会話が返信の無いまま終わっているか。真なら応答を生成する操作を出す。
  const [lacksReply, setLacksReply] = useState(false)
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
    onWheel: onLogWheel,
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
      setLacksReply(false)
    },
    [stick],
  )

  useEffect(() => {
    // oxlint-disable-next-line react/set-state-in-effect -- IPCで読み込む。stateはawaitの後で変える
    void loadTasks()
  }, [loadTasks])

  const requests = useChatRequests()
  const { reloaded } = requests
  // 狭い窓で畳むサイドバー。サイドバーで会話・タスクの追加・設定を選んだら閉じる。
  const drawer = useDrawer()
  const swipeToOpen = useSwipeToOpen(drawer)

  const loadChat = useCallback(
    async (target: Chat) => {
      const key = chatKey(target)
      try {
        const [detail, history, lacking] = await Promise.all([
          target.kind === 'task' ? getTaskDetail(target.task_id) : null,
          listChatMessages(target),
          chatLacksReply(target),
        ])
        // 読み込み中に別の会話へ移っていたら捨てる。追い越した結果で表示を上書きしない。
        if (selectedRef.current !== key) return
        setTask(detail)
        setMessages(history)
        setLacksReply(lacking)
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
    // oxlint-disable-next-line react/set-state-in-effect -- IPCで読み込む。stateはawaitの後で変える
    void loadChat(chat)
  }, [chat, loadChat])

  // 作ったらユーザーの発言を待たずに聞き取りを始める(Rust側の1つの操作)。作ったタスクの
  // 会話を開き、聞き取りの応答待ちを会話ごとの表示に載せる。
  const addTask = async () => {
    if (adding) return
    setAdding(true)
    setAddBlocked(null)
    // 聞き取りの途中経過の行き先。会話を開くまでは無い。
    let forward: ((event: TurnEvent) => void) | null = null
    let opened = false
    // 作った知らせはコマンドの完了より遅れて届くことがあるので、知らせと完了の早いほうで
    // 1回だけ開く。
    const open = (task: Task) => {
      if (opened) return
      opened = true
      setAdding(false)
      void loadTasks()
      selectChat(taskChat(task.id))
      void requests.run(
        taskChat(task.id),
        [{ role: 'pending', content: t('chat.pending_reply') }],
        (onEvent) => {
          forward = onEvent
          // 聞き取りを始められなかった理由は、その会話の失敗として出す。
          return creation.then((result) => {
            if (result.status === 'created' && result.opening_error !== null) {
              throw result.opening_error
            }
          })
        },
        settle,
      )
    }
    const creation = createTask((event) => {
      if (event.type === 'created') open(event.task)
      else forward?.(event.event)
    })
    try {
      const result = await creation
      if (result.status === 'created') {
        open(result.task)
      } else {
        // モデル未選択等でチャットを使えない間は作らない。理由はエラー発言と同じ文言で出す。
        setAddBlocked(turnErrorText(result.error_kind, result.error_kind))
      }
    } catch (e) {
      // 作る前の失敗だけが届く(作ったあとの失敗は結果に添えられる)。
      setError(failureText(e))
    } finally {
      setAdding(false)
    }
  }

  // 応答待ちの会話では、送信・編集・再試行・削除のすべてを不可にする。他の会話は応答待ちの
  // 間も操作できる。
  const disableActions = requests.isBusy(chat)

  // 送れるかは入力欄(`ChatCompose`)が判定済み。
  const send = async ({ text, attachments, restore }: ComposedMessage) => {
    const target = chat
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
          restore()
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

  // 添付は新しい発言へ引き継がれるので、添付のある発言は本文を空にしても送れる。空白だけで
  // 確定させないのは入力の補助で、受け付けるかと前後の空白を削るかはRust側が決める。
  const submitEdit = async (message: MessageView) => {
    const messageId = message.id
    const text = editDraft
    if ((text.trim() === '' && message.attachments.length === 0) || disableActions) return
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

  // 返信の無いまま終わった会話に応答を生成する。何も消さないので、画面から外す行は無い。
  const generateReply = async () => {
    if (disableActions) return
    const target = chat
    stick()
    setLacksReply(false)
    await requests.run(
      target,
      [{ role: 'pending', content: t('chat.pending_reply') }],
      (onEvent) => generateChatReply(target, onEvent),
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

  // 入力欄の「応答を生成」。返信の無いまま終わった会話には新しく生成し、最後のターンをユーザーが
  // 止めていたら、そのターンを作り直す。
  const stoppedId = stoppedTurnAtEnd(messages)
  const generateAction = lacksReply
    ? () => void generateReply()
    : stoppedId !== null
      ? () => void retry(stoppedId)
      : null

  const pending = requests.pendingOf(chat)
  const live = requests.liveOf(chat)
  const failure = requests.failureOf(chat)

  // 会話欄の中身が変わるのは、発言の引き直し・楽観表示の出し入れ・途中経過の到着・失敗の
  // 表示のとき。設定画面から戻ったときは会話欄が作り直されて先頭に戻るので、それも含める。
  useLayoutEffect(follow, [follow, messages, pending.length, live, failure, settingsOpen])

  const closeSettings = () => {
    stick()
    setAddBlocked(null)
    setSettingsOpen(false)
  }
  // Androidの「戻る」で、設定画面からチャットへ戻る。設定画面の中で開いたものが先に閉じる。
  useCloseOnBack(settingsOpen, closeSettings)

  // 通知を押して届いた会話を開く(Android)。どの会話を開くかはRust側が決めて知らせてくる。
  // 設定画面とサイドバーは、開いていれば閉じる。通知が出るのは裏にいた間なので、一覧も引き直す。
  const openRequestedChat = useEffectEvent((next: Chat) => {
    if (settingsOpen) closeSettings()
    drawer.close()
    selectChat(next)
    void loadTasks()
  })
  useEffect(() => {
    // 知らせ先は1つで、渡し直すと置き換わる(StrictModeで2回渡しても、後のものだけが残る)。
    watchRequestedChats(openRequestedChat).catch(() => undefined)
  }, [])

  if (settingsOpen) {
    return <Settings onClose={closeSettings} />
  }

  const drawerToggle = <DrawerToggle drawer={drawer} label={t('sidebar.open_tooltip')} />

  return (
    <div className={drawer.narrow ? 'layout narrow' : 'layout'}>
      <Drawer drawer={drawer}>
        <Sidebar
          tasks={tasks}
          selected={chat}
          onSelect={(next) => {
            drawer.close()
            selectChat(next)
          }}
          onAddTask={() => {
            drawer.close()
            void addTask()
          }}
          adding={adding}
          onOpenSettings={() => {
            drawer.close()
            setSettingsOpen(true)
          }}
        />
      </Drawer>

      <main inert={drawer.shown} {...swipeToOpen}>
        {task ? (
          <TaskHeader
            key={task.id}
            task={task}
            drawerToggle={drawerToggle}
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
            <div className="chat-header-row">
              {drawerToggle}
              <h1>{chat.kind === 'general' ? t('chat.general_title') : t('common.app_name')}</h1>
            </div>
          </header>
        )}

        {error && <p className="error">{error}</p>}
        {addBlocked && <p className="error">{addBlocked}</p>}

        <ChatLog
          logRef={logRef}
          onScroll={onLogScroll}
          onWheel={onLogWheel}
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

        <ChatCompose
          disabled={disableActions}
          generating={requests.isGenerating(chat)}
          stopping={requests.isStopping(chat)}
          onSend={(message) => void send(message)}
          onGenerateReply={generateAction}
          onStop={stop}
          onError={setError}
          onModelChanged={() => setAddBlocked(null)}
        />
      </main>
    </div>
  )
}
