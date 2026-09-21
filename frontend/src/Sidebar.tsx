import { useState } from 'react'
import type { TaskSummary } from './types'

interface SidebarProps {
  tasks: TaskSummary[]
  selectedTaskId: number | null
  onSelect: (taskId: number) => void
  onAddTask: () => void
  adding: boolean
  onOpenSettings: () => void
}

function taskLabel(task: TaskSummary): string {
  const progress =
    task.steps_total > 0 ? `${task.steps_done}/${task.steps_total}完了` : '工程なし'
  const deadline = task.deadline ?? '締切未設定'
  return `${task.title ?? '(無題)'} · ${deadline} · ${progress}`
}

// サイドバー: 総合チャット行(固定・現時点では無効)・タスク一覧・アーカイブ折りたたみ・
// 新規タスク追加ボタン(legacy/frontend.md 1節)。総合チャット自体は別Issue。
export default function Sidebar({
  tasks,
  selectedTaskId,
  onSelect,
  onAddTask,
  adding,
  onOpenSettings,
}: SidebarProps) {
  const [archivedOpen, setArchivedOpen] = useState(false)

  const active = tasks.filter((t) => t.archived_at === null)
  const archived = tasks.filter((t) => t.archived_at !== null)

  return (
    <nav className="sidebar">
      <div className="sidebar-general-row">
        <button type="button" className="sidebar-general" disabled>
          総合
        </button>
        <button
          type="button"
          className="sidebar-settings-icon"
          onClick={onOpenSettings}
          aria-label="設定"
          title="設定"
        >
          ⚙
        </button>
      </div>

      {/* 総合行と新規タスクを常に見える位置に留めるため、スクロールするのはここだけ */}
      <div className="sidebar-scroll">
        <ul className="sidebar-task-list">
          {active.map((task) => (
            <li key={task.id}>
              <button
                type="button"
                className={task.id === selectedTaskId ? 'sidebar-task selected' : 'sidebar-task'}
                onClick={() => onSelect(task.id)}
              >
                {taskLabel(task)}
              </button>
            </li>
          ))}
        </ul>

        {archived.length > 0 && (
          <div className="sidebar-archived">
            <button
              type="button"
              className="sidebar-archived-toggle"
              onClick={() => setArchivedOpen((open) => !open)}
            >
              アーカイブ済み({archived.length}){archivedOpen ? ' ▲' : ' ▼'}
            </button>
            {archivedOpen && (
              <ul className="sidebar-task-list">
                {archived.map((task) => (
                  <li key={task.id}>
                    <button
                      type="button"
                      className={
                        task.id === selectedTaskId ? 'sidebar-task selected' : 'sidebar-task'
                      }
                      onClick={() => onSelect(task.id)}
                    >
                      {taskLabel(task)}
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}
      </div>

      <button type="button" className="sidebar-add" onClick={onAddTask} disabled={adding}>
        + 新規タスク
      </button>
    </nav>
  )
}
