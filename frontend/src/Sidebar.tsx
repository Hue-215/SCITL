import { useState } from 'react'
import { GENERAL_CHAT, taskChat } from './chat'
import { t } from './i18n'
import { taskName, taskProgress } from './taskName'
import type { Chat, TaskSummary } from './types'

interface SidebarProps {
  tasks: TaskSummary[]
  selected: Chat
  onSelect: (chat: Chat) => void
  onAddTask: () => void
  adding: boolean
  onOpenSettings: () => void
}

function taskLabel(task: TaskSummary): string {
  return t('sidebar.task_label', { title: taskName(task), ...taskProgress(task) })
}

function TaskList({
  tasks,
  selectedTaskId,
  onSelect,
}: {
  tasks: TaskSummary[]
  selectedTaskId: number | null
  onSelect: (chat: Chat) => void
}) {
  return (
    <ul className="sidebar-task-list">
      {tasks.map((task) => (
        <li key={task.id}>
          <button
            type="button"
            className={task.id === selectedTaskId ? 'list-row selected' : 'list-row'}
            onClick={() => onSelect(taskChat(task.id))}
          >
            {taskLabel(task)}
          </button>
        </li>
      ))}
    </ul>
  )
}

// サイドバー: 総合チャット行(固定)・タスク一覧・アーカイブ折りたたみ・
// 新規タスク追加ボタン(legacy/frontend.md 1節)。
export default function Sidebar({
  tasks,
  selected,
  onSelect,
  onAddTask,
  adding,
  onOpenSettings,
}: SidebarProps) {
  const [archivedOpen, setArchivedOpen] = useState(false)

  const active = tasks.filter((t) => t.archived_at === null)
  const archived = tasks.filter((t) => t.archived_at !== null)
  const selectedTaskId = selected.kind === 'task' ? selected.task_id : null

  return (
    <nav className="sidebar">
      <div className="sidebar-general-row">
        <button
          type="button"
          className={selected.kind === 'general' ? 'sidebar-general selected' : 'sidebar-general'}
          onClick={() => onSelect(GENERAL_CHAT)}
        >
          {t('sidebar.general_chat')}
        </button>
        <button
          type="button"
          className="icon-button"
          onClick={onOpenSettings}
          aria-label={t('sidebar.settings_tooltip')}
          title={t('sidebar.settings_tooltip')}
        >
          ⚙
        </button>
      </div>

      {/* 総合行と新規タスクを常に見える位置に留めるため、スクロールするのはここだけ */}
      <div className="sidebar-scroll">
        <TaskList tasks={active} selectedTaskId={selectedTaskId} onSelect={onSelect} />

        {/* 0件でも行は出す(legacy/frontend.md 1節)。アーカイブした行き先が常に見えるように */}
        <div className="sidebar-archived">
          <button
            type="button"
            className="list-row sidebar-archived-toggle"
            onClick={() => setArchivedOpen((open) => !open)}
          >
            {t('sidebar.archived_label', { count: archived.length })}
            {archivedOpen ? ' ▲' : ' ▼'}
          </button>
          {archivedOpen && (
            <TaskList tasks={archived} selectedTaskId={selectedTaskId} onSelect={onSelect} />
          )}
        </div>
      </div>

      <button type="button" className="sidebar-add" onClick={onAddTask} disabled={adding}>
        + {t('sidebar.new_task_button')}
      </button>
    </nav>
  )
}
