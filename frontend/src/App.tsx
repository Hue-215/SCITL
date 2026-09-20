import { useCallback, useEffect, useState } from 'react'
import { createTask, getTaskDetail, listTasks, sendTaskChatMessage } from './api'
import Sidebar from './Sidebar'
import type { ChatEntry, Task, TaskSummary } from './types'

export default function App() {
  const [tasks, setTasks] = useState<TaskSummary[]>([])
  const [taskId, setTaskId] = useState<number | null>(null)
  const [adding, setAdding] = useState(false)
  const [task, setTask] = useState<Task | null>(null)
  const [entries, setEntries] = useState<ChatEntry[]>([])
  const [draft, setDraft] = useState('')
  const [sending, setSending] = useState(false)
  const [error, setError] = useState<string | null>(null)

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
      setTask(await getTaskDetail(id))
      setEntries([])
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
    setEntries((prev) => [...prev, { role: 'user', content: text }])
    setSending(true)
    setError(null)
    try {
      const events = await sendTaskChatMessage(taskId, text)
      let assistantText = ''
      const newEntries: ChatEntry[] = []
      for (const event of events) {
        if (event.type === 'text_delta') {
          assistantText += event.text
        } else if (event.type === 'tool_call') {
          newEntries.push({
            role: 'tool',
            content: `ツール実行: ${event.name}`,
          })
        }
      }
      if (assistantText) {
        newEntries.push({ role: 'assistant', content: assistantText })
      }
      setEntries((prev) => [...prev, ...newEntries])
      await loadTask(taskId)
      await loadTasks()
    } catch (e) {
      setError(String(e))
    } finally {
      setSending(false)
    }
  }

  return (
    <div className="layout">
      <Sidebar
        tasks={tasks}
        selectedTaskId={taskId}
        onSelect={setTaskId}
        onAddTask={() => void addTask()}
        adding={adding}
      />

      <main>
        <header>
          <h1>{task ? (task.title ?? '(無題)') : 'SCITL'}</h1>
          {task?.description && <p>{task.description}</p>}
        </header>

        {error && <p className="error">{error}</p>}

        <ul className="chat-log">
          {entries.map((entry, i) => (
            <li key={i} className={`entry entry-${entry.role}`}>
              {entry.content}
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
