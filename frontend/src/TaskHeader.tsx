import { useState, type ReactNode } from 'react'
import { MAX_TITLE_CHARS } from './bindings/SharedConstants'
import { ConfirmDialog } from './Dialog'
import { isolated, t } from './i18n'
import { DisclosureMark } from './Icon'
import { isCommitEnter } from './keyboard'
import MenuButton from './MenuButton'
import { taskName, taskProgress } from './taskName'
import type { TaskDetailView } from './types'

// タイトルの上限文字数(`MAX_TITLE_CHARS`。Rust側の値を`cargo test`が書き出したもの)で、超えた
// 値はRust側が断る。入力欄で先に止めて、打った名前が断られる前に上限に気付けるようにする。
// 長い名前を貼り付けると入力欄が切るが、切った値は確定(Enter)の前に入力欄に見えているので、
// 黙って切ることにはならない。`maxLength`はUTF-16の単位で、整形(前後の括弧等を除く)の前の値を
// 数えるので、Rust側では通る値でも手前で止まることがある(絵文字を含む・括弧で囲んだ等)。

interface TaskHeaderProps {
  task: TaskDetailView
  // 名前の前に置く、畳んだサイドバーを引き出すボタン(Drawer.tsx)。
  drawerToggle: ReactNode
  // 応答待ちの間は操作させない(Rust側でも断る。orchestration::operations)。
  disabled: boolean
  onRename: (title: string) => void
  onSetArchived: (archived: boolean) => void
  onDelete: () => void
}

// タスクチャットのヘッダー。名前のその場での変更・締切と工程の進捗・説明の開け閉め・
// アーカイブの切り替えと削除(︙のメニュー)。タスクを切り替えたら編集中の状態を捨てるよう、呼び出し側はタスクのidを`key`に渡す。
export default function TaskHeader({
  task,
  drawerToggle,
  disabled,
  onRename,
  onSetArchived,
  onDelete,
}: TaskHeaderProps) {
  const [editing, setEditing] = useState(false)
  const [draft, setDraft] = useState('')
  // 説明は場所を取るので畳んでおき、進捗の行の「説明」で開く。
  const [descriptionOpen, setDescriptionOpen] = useState(false)
  const [confirmingDelete, setConfirmingDelete] = useState(false)
  const name = taskName(task)
  const archived = task.archived_at !== null

  // 空・変更なしは取り消しと同じ扱いにする(何も変わらない記録を残さない)。整形すると今の
  // タイトルと同じになる値(括弧で囲んだ等)は、Rust側が変更も記録もしない
  // (orchestration::operations)。整形の規則はここに写さない。
  const commit = () => {
    setEditing(false)
    const title = draft.trim()
    if (title !== '' && title !== task.title) onRename(title)
  }

  return (
    <header className="chat-header">
      <div className="chat-header-row">
        {drawerToggle}
        <h1>
          {editing ? (
            <input
              className="chat-header-title-input"
              value={draft}
              maxLength={MAX_TITLE_CHARS}
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
              // フォーカスを外したら取り消す。確定はEnterだけ。
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
        {/* 名前の長さや編集中かによらず、右上の同じ位置に置く。名前の変更は名前を押して行うので
            ここには入れない。編集中に押すと、名前の欄からフォーカスが外れて編集を取り消してから開く */}
        <MenuButton
          label={t('task_header.menu_label')}
          disabled={disabled}
          items={[
            {
              key: 'archive',
              label: archived ? t('task_header.unarchive') : t('task_header.archive'),
              onSelect: () => onSetArchived(!archived),
            },
            {
              key: 'delete',
              label: t('common.delete'),
              danger: true,
              onSelect: () => setConfirmingDelete(true),
            },
          ]}
        />
      </div>
      <div className="chat-header-meta">
        {task.description && (
          <button
            type="button"
            className="chat-header-description-toggle"
            aria-expanded={descriptionOpen}
            onClick={() => setDescriptionOpen((open) => !open)}
          >
            <DisclosureMark />
            {t('task_header.description_toggle')}
          </button>
        )}
        {/* 説明を開くと左端から下へ続くので、開け閉めする行を左に、締切と進捗を右に置く */}
        <p>{t('task_header.meta', taskProgress(task))}</p>
      </div>
      {task.description && descriptionOpen && (
        <p>{task.description}</p>
      )}
      {confirmingDelete && (
        <ConfirmDialog
          title={t('task_header.delete_dialog_title')}
          message={t('task_header.delete_dialog_message', { title: isolated(name) })}
          confirmLabel={t('common.delete')}
          onCancel={() => setConfirmingDelete(false)}
          onConfirm={() => {
            setConfirmingDelete(false)
            onDelete()
          }}
        />
      )}
    </header>
  )
}
