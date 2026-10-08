// 設定画面の「メモリ」タブ。モデルが会話をまたいで覚えている、利用者についての事実の一覧・
// 書き足し・編集・削除。
import { useEffect, useId, useState } from 'react'
import { addMemory, deleteMemory, failureText, listMemories, updateMemory } from './api'
import { MAX_MEMORIES, MAX_MEMORY_CHARS } from './bindings/SharedConstants'
import type { Memory } from './types'
import { ConfirmButton } from './Dialog'
import { formatDateTime, isolated, t } from './i18n'
import { useAsyncAction } from './useAsyncAction'

// 文字数はRust側と同じくUnicodeのスカラー値で数える(`maxLength`はUTF-16の単位なので使わない)。
function charCount(text: string): number {
  return Array.from(text).length
}

interface DraftFieldProps {
  label: string
  value: string
  onChange: (value: string) => void
  placeholder?: string
}

// 本文の入力欄と文字数。上限を超えたら欄と文字数に誤りの色を付ける(切り詰めはしない)。
function DraftField({ label, value, onChange, placeholder }: DraftFieldProps) {
  const countId = useId()
  const count = charCount(value)
  const over = count > MAX_MEMORY_CHARS
  return (
    <label className="settings-field">
      <span>{label}</span>
      <textarea
        value={value}
        placeholder={placeholder}
        aria-invalid={over}
        aria-describedby={countId}
        onChange={(e) => onChange(e.target.value)}
      />
      <span id={countId} className={over ? 'settings-hint error' : 'settings-hint'}>
        {t('settings.memory.char_count', { count, max: MAX_MEMORY_CHARS })}
      </span>
    </label>
  )
}

// 送れる本文か。空と上限超えはRust側も断るが、押せる状態にしないために画面でも見る。
function canSubmit(text: string): boolean {
  return text.trim() !== '' && charCount(text) <= MAX_MEMORY_CHARS
}

interface MemoryItemProps {
  memory: Memory
  onChanged: () => Promise<void>
}

function MemoryItem({ memory, onChanged }: MemoryItemProps) {
  const [draft, setDraft] = useState<string | null>(null)
  const saving = useAsyncAction()
  const removing = useAsyncAction()

  const save = (text: string) =>
    void saving.run(
      async () => {
        await updateMemory(memory.id, text)
        await onChanged()
      },
      () => setDraft(null),
    )

  if (draft !== null) {
    return (
      <li className="memory-item editing">
        <DraftField label={t('settings.memory.edit_label')} value={draft} onChange={setDraft} />
        <div className="button-row memory-actions">
          <button
            type="button"
            onClick={() => {
              saving.clear()
              setDraft(null)
            }}
          >
            {t('common.cancel')}
          </button>
          <button
            type="button"
            className="primary"
            disabled={saving.running || !canSubmit(draft)}
            onClick={() => save(draft)}
          >
            {t('settings.memory.save')}
          </button>
        </div>
        {saving.error && <p className="error">{saving.error}</p>}
      </li>
    )
  }

  return (
    <li className="memory-item">
      <div className="memory-item-row">
        <div className="memory-item-body">
          <p className="memory-item-content">{memory.content}</p>
          <time className="settings-hint" dateTime={memory.updated_at}>
            {t('settings.memory.updated_at', { at: formatDateTime(memory.updated_at) })}
          </time>
        </div>
        <div className="button-row">
          <button
            type="button"
            aria-label={t('settings.memory.edit_aria', { content: isolated(memory.content) })}
            onClick={() => setDraft(memory.content)}
          >
            {t('settings.memory.edit')}
          </button>
          <ConfirmButton
            label={t('common.delete')}
            confirmTitle={t('settings.memory.delete_dialog_title')}
            confirmMessage={t('settings.memory.delete_dialog_message', {
              content: isolated(memory.content),
            })}
            confirmLabel={t('common.delete')}
            disabled={removing.running}
            onConfirm={() =>
              void removing.run(async () => {
                await deleteMemory(memory.id)
                await onChanged()
              })
            }
          />
        </div>
      </div>
      {removing.error && <p className="error">{removing.error}</p>}
    </li>
  )
}

export function MemoryTab() {
  const [memories, setMemories] = useState<Memory[] | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [draft, setDraft] = useState('')
  const adding = useAsyncAction()
  const headingId = useId()

  const reload = async () => {
    try {
      setMemories(await listMemories())
      setLoadError(null)
    } catch (e) {
      setLoadError(failureText(e))
    }
  }

  useEffect(() => {
    // oxlint-disable-next-line react/set-state-in-effect -- IPCで読み込む。stateはawaitの後で変える
    void reload()
  }, [])

  const add = (e: React.FormEvent) => {
    e.preventDefault()
    void adding.run(
      async () => {
        await addMemory(draft)
        await reload()
      },
      () => setDraft(''),
    )
  }

  return (
    <div className="settings-panel">
      <div className="settings-field">
        <p>{t('settings.memory.intro')}</p>
        <p className="settings-hint">{t('settings.memory.intro_hint')}</p>
      </div>

      <section className="settings-field" aria-labelledby={headingId}>
        <div className="memory-list-header">
          <h2 id={headingId}>{t('settings.memory.list_heading')}</h2>
          {memories && (
            <span className="settings-hint">
              {t('settings.memory.count', { count: memories.length, max: MAX_MEMORIES })}
            </span>
          )}
        </div>
        {loadError && <p className="error">{loadError}</p>}
        {memories === null ? (
          !loadError && <p>{t('common.loading')}</p>
        ) : memories.length === 0 ? (
          <p className="memory-empty">{t('settings.memory.empty')}</p>
        ) : (
          <ul className="memory-list">
            {memories.map((memory) => (
              <MemoryItem key={memory.id} memory={memory} onChanged={reload} />
            ))}
          </ul>
        )}
      </section>

      <form className="settings-section settings-section-break" onSubmit={add}>
        <DraftField
          label={t('settings.memory.add_label')}
          value={draft}
          onChange={setDraft}
          placeholder={t('settings.memory.add_placeholder')}
        />
        <div className="button-row memory-actions">
          <button
            type="submit"
            className="primary"
            disabled={adding.running || !canSubmit(draft)}
          >
            {adding.running ? t('common.adding') : t('common.add')}
          </button>
        </div>
        {adding.error && <p className="error">{adding.error}</p>}
      </form>
    </div>
  )
}
