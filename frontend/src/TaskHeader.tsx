import { useState } from 'react'
import { ConfirmButton } from './Dialog'
import { t } from './i18n'
import { isCommitEnter } from './keyboard'
import { taskName, taskProgress } from './taskName'
import type { TaskDetail } from './types'

interface TaskHeaderProps {
  task: TaskDetail
  // 応答待ちの間は操作させない(Rust側でも断る。orchestration::operations)。
  disabled: boolean
  onRename: (title: string) => void
  onSetArchived: (archived: boolean) => void
  onDelete: () => void
}

// タスクチャットのヘッダー(Issue #75、legacy/frontend.md 1節)。名前のその場での変更・
// 締切と工程の進捗・アーカイブの切り替え・削除。タスクを切り替えたら編集中の状態を捨てるよう、
// 呼び出し側はタスクのidを`key`に渡す。
export default function TaskHeader({
  task,
  disabled,
  onRename,
  onSetArchived,
  onDelete,
}: TaskHeaderProps) {
  const [editing, setEditing] = useState(false)
  const [draft, setDraft] = useState('')
  const name = taskName(task)
  const archived = task.archived_at !== null

  // 空・変更なしは取り消しと同じ扱いにする(何も変わらない記録を残さない)。
  const commit = () => {
    setEditing(false)
    const title = draft.trim()
    if (title !== '' && title !== task.title) onRename(title)
  }

  return (
    <header className="chat-header">
      <div className="chat-header-row">
        <h1>
          {editing ? (
            <input
              className="chat-header-title-input"
              value={draft}
              // 未設定のタスクは、画面で呼んでいる名前(フォールバック)を手掛かりに出す。
              placeholder={name}
              aria-label={t('task_header.title_input_label')}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (isCommitEnter(e)) {
                  e.preventDefault()
                  commit()
                } else if (e.key === 'Escape') {
                  setEditing(false)
                }
              }}
              // フォーカスを外したら取り消す(legacy/frontend.md 1節)。確定はEnterだけ。
              onBlur={() => setEditing(false)}
              autoFocus
            />
          ) : (
            <button
              type="button"
              className="chat-header-title"
              disabled={disabled}
              title={t('task_header.rename_hint')}
              onClick={() => {
                setDraft(task.title ?? '')
                setEditing(true)
              }}
            >
              {name}
            </button>
          )}
        </h1>
        <div className="chat-header-actions">
          <button type="button" disabled={disabled} onClick={() => onSetArchived(!archived)}>
            {archived ? t('task_header.unarchive') : t('task_header.archive')}
          </button>
          <ConfirmButton
            label={t('common.delete')}
            confirmTitle={t('task_header.delete_dialog_title')}
            confirmMessage={t('task_header.delete_dialog_message', { title: name })}
            confirmLabel={t('common.delete')}
            onConfirm={onDelete}
            disabled={disabled}
          />
        </div>
      </div>
      <p className="chat-header-meta">{t('task_header.meta', taskProgress(task))}</p>
      {task.description && <p>{task.description}</p>}
    </header>
  )
}
