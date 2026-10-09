import { useCallback, useEffect, useState } from 'react'
import { failureText, getChatModels, selectChatModel, setReasoningEffort } from './api'
import Dropdown, { type DropdownOption } from './Dropdown'
import { isolated, type MessageKey, t } from './i18n'
import type { AttachmentDeliveries, ChatModelsView, ModelChoice, ReasoningEffort } from './types'

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

// チャット入力欄の下の行(添付と送信の間)に置くモデル選択と思考の強さ選択。選べるものと選択中の
// ものはRust側が組み立てて渡し、ここは描いて選ばせるだけ。
export default function ChatModelBar({
  onError,
  onChanged,
  onDeliveries,
}: {
  onError: (message: string) => void
  // 選択を変えられたとき。
  onChanged: () => void
  // 添付の渡し方を読み込むたび。入力欄の添付の警告に使う。
  onDeliveries: (deliveries: AttachmentDeliveries) => void
}) {
  const [view, setView] = useState<ChatModelsView | null>(null)

  const reload = useCallback(async () => {
    try {
      const next = await getChatModels()
      setView(next)
      onDeliveries(next.attachments)
    } catch (e) {
      onError(failureText(e))
    }
  }, [onError, onDeliveries])

  useEffect(() => {
    // oxlint-disable-next-line react/set-state-in-effect -- IPCで読み込む。stateはawaitの後で変える
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

  return (
    // 一覧の位置と幅の基準(Dropdown.tsx)は、モデル選択では自分の入れ物全体、思考の強さ選択では
    // その中の自分から右の部分にする。どちらの一覧もボタンの左端から開き、右の送信ボタンの手前まで
    // 広がれる。
    <div className="chat-model-bar">
      <Dropdown
        toggleClassName="chat-model-toggle"
        label={selected ? selected.label : t('select.model_unset')}
        title={
          selected
            ? t('select.model_tooltip', {
                model: isolated(selected.label),
                provider: isolated(selected.provider_name),
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
        emptyText={t('select.models_empty')}
        direction="up"
        align="start"
      />
      <div className="chat-model-effort">
        <Dropdown
          toggleClassName="chat-model-toggle"
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
          direction="up"
          align="start"
        />
      </div>
    </div>
  )
}
