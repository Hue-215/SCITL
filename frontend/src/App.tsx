import { useCallback, useEffect, useState } from 'react'
import {
  createTask,
  getTaskDetail,
  listTaskMessages,
  listTasks,
  sendTaskChatMessage,
} from './api'
import Settings from './Settings'
import Sidebar from './Sidebar'
import { ExternalToolLine, ThinkingTools } from './ThinkingTools'
import { finalEntryOf, groupMessages } from './thinking'
import type { Message, PendingEntry, Task, TaskSummary } from './types'

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
              return (
                <li key={message.id} className={`entry entry-${message.role}`}>
                  <span className="entry-content">{message.content}</span>
                  <time className="entry-time">{formatTime(message.created_at)}</time>
                </li>
              )
            }

            // SCITL自身の応答生成1ターン分。思考・内部ツール呼び出しを発生順の折りたたみで
            // 見せたうえで、実際の返信(最終行)を通常の吹き出しとして表示する(Issue #42)。
            const finalMessage = finalEntryOf(item.entries)
            return (
              <li key={`turn-${item.turnId}`} className="turn-group">
                <ThinkingTools entries={item.entries} />
                <div className={`entry entry-${finalMessage.role}`}>
                  <span className="entry-content">{finalMessage.content}</span>
                  <time className="entry-time">{formatTime(finalMessage.created_at)}</time>
                </div>
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
