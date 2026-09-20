import { useCallback, useEffect, useState } from 'react'
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
import Settings from './Settings'
import Sidebar from './Sidebar'
import type { Message, PendingEntry, Task, TaskSummary } from './types'

function toolSummary(content: string): string {
  try {
    const parsed = JSON.parse(content) as { tool?: string }
    return `ツール実行: ${parsed.tool ?? '不明'}`
  } catch {
    return 'ツール実行'
  }
}

function formatTime(createdAt: string): string {
  return new Date(createdAt).toLocaleString()
}

export default function App() {
  const [tasks, setTasks] = useState<TaskSummary[]>([])
  const [taskId, setTaskId] = useState<number | null>(null)
  const [adding, setAdding] = useState(false)
  const [task, setTask] = useState<Task | null>(null)
  const [messages, setMessages] = useState<Message[]>([])
  const [pending, setPending] = useState<PendingEntry[]>([])
  const [draft, setDraft] = useState('')
  const [sending, setSending] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [settingsOpen, setSettingsOpen] = useState(false)
  // 編集モード(Issue #41)。ユーザー発言のみが対象。応答待ち中は開始できない
  // (`disableActions`参照)。
  const [editingId, setEditingId] = useState<number | null>(null)
  const [editDraft, setEditDraft] = useState('')

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

  useEffect(() => {
    void loadTasks().then((summaries) => {
      if (summaries.length > 0) setTaskId(summaries[0].id)
    })
  }, [loadTasks])

  const loadTask = useCallback(async (id: number) => {
    try {
      const [detail, history] = await Promise.all([getTaskDetail(id), listTaskMessages(id)])
      setTask(detail)
      setMessages(history)
      setPending([])
      setEditingId(null)
      setError(null)
    } catch (e) {
      setError(String(e))
    }
  }, [])

  useEffect(() => {
    if (taskId !== null) void loadTask(taskId)
  }, [taskId, loadTask])

  const addTask = async () => {
    if (adding) return
    setAdding(true)
    try {
      const created = await createTask()
      await loadTasks()
      setTaskId(created.id)
      setError(null)
    } catch (e) {
      setError(String(e))
    } finally {
      setAdding(false)
    }
  }

  const send = async () => {
    const text = draft.trim()
    if (!text || sending || taskId === null) return
    setDraft('')
    // 楽観表示はユーザー発言と応答待ちプレースホルダのみに留め、応答本体は確定後に
    // DBから引き直す(docs/spec/principles.md 3節「保存するのは組み立て終わった応答」)。
    setPending([
      { role: 'user', content: text },
      { role: 'pending', content: '応答待ち…' },
    ])
    setSending(true)
    setError(null)
    try {
      await sendTaskChatMessage(taskId, text)
      await loadTask(taskId)
      await loadTasks()
    } catch (e) {
      setError(String(e))
      await loadTask(taskId)
    } finally {
      setSending(false)
    }
  }

  // 応答待ち中は編集・再試行・削除のすべてを不可にする(Issue #41、legacy/frontend.md 1節)。
  const disableActions = sending || taskId === null

  const submitEdit = async (messageId: number) => {
    const text = editDraft.trim()
    if (!text || disableActions || taskId === null) return
    setEditingId(null)
    setPending([
      { role: 'user', content: text },
      { role: 'pending', content: '応答待ち…' },
    ])
    setSending(true)
    setError(null)
    try {
      await editTaskChatMessage(taskId, messageId, text)
      await loadTask(taskId)
      await loadTasks()
    } catch (e) {
      setError(String(e))
      await loadTask(taskId)
    } finally {
      setSending(false)
    }
  }

  const retry = async (messageId: number) => {
    if (disableActions || taskId === null) return
    setPending([{ role: 'pending', content: '応答待ち…' }])
    setSending(true)
    setError(null)
    try {
      await retryTaskChatMessage(taskId, messageId)
      await loadTask(taskId)
      await loadTasks()
    } catch (e) {
      setError(String(e))
      await loadTask(taskId)
    } finally {
      setSending(false)
    }
  }

  const remove = async (messageId: number) => {
    // 確認ダイアログ無しの即座に取り消し可能な論理削除(legacy/frontend.md 1節)。
    if (disableActions || taskId === null) return
    setError(null)
    try {
      await deleteTaskChatMessage(taskId, messageId)
      await loadTask(taskId)
    } catch (e) {
      setError(String(e))
    }
  }

  if (settingsOpen) {
    return <Settings onClose={() => setSettingsOpen(false)} />
  }

  return (
    <div className="layout">
      <Sidebar
        tasks={tasks}
        selectedTaskId={taskId}
        onSelect={setTaskId}
        onAddTask={() => void addTask()}
        adding={adding}
        onOpenSettings={() => setSettingsOpen(true)}
      />

      <main>
        <header>
          <h1>{task ? (task.title ?? '(無題)') : 'SCITL'}</h1>
          {task?.description && <p>{task.description}</p>}
        </header>

        {error && <p className="error">{error}</p>}

        <ul className="chat-log">
          {messages.map((message) => {
            // 編集・再試行・削除(Issue #41)。対象はツール実行記録を除く通常発言のみ
            // (data-model.md「ツール実行記録は通常発言の編集・削除・再試行の対象に
            // 含めない」)。編集はユーザー発言のみ、再試行はアシスタント発言のみ、
            // 削除は両方に共通(legacy/frontend.md 1節)。
            const isNormal = message.kind === 'normal'
            const canEdit = isNormal && message.role === 'user'
            const canRetry = isNormal && message.role === 'assistant'
            const canDelete = isNormal && (message.role === 'user' || message.role === 'assistant')

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
                <span className="entry-content">
                  {message.kind === 'tool_execution' ? toolSummary(message.content) : message.content}
                </span>
                <time className="entry-time">{formatTime(message.created_at)}</time>
                {(canEdit || canRetry || canDelete) && (
                  <div className="entry-actions">
                    {canEdit && (
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
                    )}
                    {canRetry && (
                      <button
                        type="button"
                        disabled={disableActions}
                        onClick={() => void retry(message.id)}
                      >
                        再試行
                      </button>
                    )}
                    {canDelete && (
                      <button
                        type="button"
                        disabled={disableActions}
                        onClick={() => void remove(message.id)}
                      >
                        削除
                      </button>
                    )}
                  </div>
                )}
              </li>
            )
          })}
          {pending.map((entry, i) => (
            <li key={`pending-${i}`} className={`entry entry-${entry.role}`}>
              <span className="entry-content">{entry.content}</span>
            </li>
          ))}
        </ul>

        <form
          onSubmit={(e) => {
            e.preventDefault()
            void send()
          }}
        >
          <input
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            disabled={sending || taskId === null}
            placeholder="タスクについて話しかける"
          />
          <button type="submit" disabled={sending || taskId === null || !draft.trim()}>
            送信
          </button>
        </form>
      </main>
    </div>
  )
}
