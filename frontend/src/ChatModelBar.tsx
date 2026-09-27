import { useCallback, useEffect, useState } from 'react'
import { failureText, getChatModels, selectChatModel, setReasoningEffort } from './api'
import Dropdown, { type DropdownOption } from './Dropdown'
import { t, type MessageKey } from './i18n'
import type { ChatModelsView, ModelChoice, ReasoningEffort, SelectedModel } from './types'

const EFFORT_LABELS: Record<ReasoningEffort, MessageKey> = {
  off: 'select.effort.off',
  low: 'select.effort.low',
  medium: 'select.effort.medium',
  high: 'select.effort.high',
}

// 一覧はボタンの上に開くので、ボタンに近い下ほど弱くする(上から高・中・低・オフ)。
function effortOptions(): DropdownOption[] {
  return Object.entries(EFFORT_LABELS)
    .reverse()
    .map(([key, label]) => ({ key, label: t(label) }))
}

// モデルはプロバイダーを跨いで並ぶので、組を1つの鍵にする。
function choiceKey(choice: ModelChoice): string {
  return JSON.stringify([choice.provider_id, choice.model])
}

// チャット入力欄の下に置くモデル選択と思考の強さ選択(Issue #64、legacy/frontend.md 1節)。
// 選べるものと選択中のものはRust側が組み立てて渡し、ここは描いて選ばせるだけ。
export default function ChatModelBar({
  onError,
  onChanged,
  onSelected,
}: {
  onError: (message: string) => void
  // 選択を変えられたとき。
  onChanged: () => void
  // 選択中のモデル(未選択ならnull)を読み込むたび。入力欄の添付の警告に使う。
  onSelected: (selected: SelectedModel | null) => void
}) {
  const [view, setView] = useState<ChatModelsView | null>(null)
  const [open, setOpen] = useState<'model' | 'effort' | null>(null)

  const reload = useCallback(async () => {
    try {
      const next = await getChatModels()
      setView(next)
      onSelected(next.selected)
    } catch (e) {
      onError(failureText(e))
    }
  }, [onError, onSelected])

  useEffect(() => {
    void reload()
  }, [reload])

  const change = async (update: () => Promise<void>) => {
    try {
      await update()
      onChanged()
    } catch (e) {
      onError(failureText(e))
    }
    await reload()
  }

  const selected = view?.selected ?? null
  const choices = view?.choices ?? []
  const toggle = (which: 'model' | 'effort') => (next: boolean) =>
    setOpen((current) => (next ? which : current === which ? null : current))

  return (
    // 左右余白は外側(mainの直接の子)が持ち、一覧の位置と幅の基準は内側の行にする。
    // 基準を外側に置くと、一覧の端がボタンではなく余白の外側に揃う。
    <div className="chat-model-bar">
      <div className="chat-model-bar-row">
        <Dropdown
          open={open === 'model'}
          onOpenChange={toggle('model')}
          label={selected ? selected.label : t('select.model_unset')}
          title={
            selected
              ? t('select.model_tooltip', {
                  model: selected.label,
                  provider: selected.provider_name,
                })
              : undefined
          }
          disabled={view === null}
          options={choices.map((c) => ({
            key: choiceKey(c),
            label: c.label,
            detail: c.provider_name,
          }))}
          selectedKey={selected && choiceKey(selected)}
          onSelect={(key) => {
            const choice = choices.find((c) => choiceKey(c) === key)
            if (choice) void change(() => selectChatModel(choice.provider_id, choice.model))
          }}
          searchPlaceholder={t('select.model_search_hint')}
          emptyText={t('select.models_empty')}
          align="start"
        />
        <Dropdown
          open={open === 'effort'}
          onOpenChange={toggle('effort')}
          label={
            !selected
              ? t('select.thinking_label')
              : t('select.thinking_effort_label', {
                  effort: selected.thinking
                    ? t(EFFORT_LABELS[selected.reasoning_effort])
                    : t('select.thinking_unsupported'),
                })
          }
          title={
            selected && !selected.thinking
              ? t('select.thinking_unsupported_tooltip')
              : t('select.thinking_tooltip')
          }
          disabled={!selected?.thinking}
          options={effortOptions()}
          selectedKey={selected?.reasoning_effort ?? null}
          onSelect={(key) => {
            if (!selected) return
            void change(() =>
              setReasoningEffort(selected.provider_id, selected.model, key as ReasoningEffort),
            )
          }}
          emptyText=""
          align="end"
        />
      </div>
    </div>
  )
}
