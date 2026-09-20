import { useCallback, useEffect, useState } from 'react'
import { getTaskDetail, sendTaskChatMessage } from './api'
import type { ChatEntry, Task } from './types'

// 縦切り検証段階の最小画面: タスクチャット1本のみ。タスク一覧・切り替えUIは別Issue。
const TASK_ID = 1

export default function App() {
  const [task, setTask] = useState<Task | null>(null)
  const [entries, setEntries] = useState<ChatEntry[]>([])
  const [draft, setDraft] = useState('')
  const [sending, setSending] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const loadTask = useCallback(async () => {
    try {
      setTask(await getTaskDetail(TASK_ID))
      setError(null)
    } catch (e) {
      setError(String(e))
    }
  }, [])

  useEffect(() => {
    void loadTask()
  }, [loadTask])

  const send = async () => {
    const text = draft.trim()
    if (!text || sending) return
    setDraft('')
    setEntries((prev) => [...prev, { role: 'user', content: text }])
    setSending(true)
    setError(null)
    try {
      const events = await sendTaskChatMessage(TASK_ID, text)
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
      await loadTask()
    } catch (e) {
      setError(String(e))
    } finally {
      setSending(false)
    }
  }

  return (
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
          disabled={sending}
          placeholder="タスクについて話しかける"
        />
        <button type="submit" disabled={sending || !draft.trim()}>
          送信
        </button>
      </form>
    </main>
  )
}
