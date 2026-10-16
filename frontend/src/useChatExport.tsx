import { useRef, useState } from 'react'
import { exportChatMarkdown, failureText, openExportFolder } from './api'
import Dialog from './Dialog'
import ExportOutcomeText from './ExportOutcomeText'
import { isolated, t } from './i18n'
import type { Chat, ExportOutcome } from './types'
import { useAsyncAction } from './useAsyncAction'

type ChatExport =
  | { status: 'running' }
  | { status: 'done'; outcome: ExportOutcome }
  | { status: 'failed'; error: string }

// 会話1つの書き出し(タスク・総合チャットの︙のメニュー)。押すと確認なしで書き出し、経過と結果を
// ダイアログに出す。保存画面を取りやめたらダイアログを閉じる。書き出し中に閉じても書き出しは
// 止まらず、結果は出さない。
export function useChatExport() {
  const [state, setState] = useState<ChatExport | null>(null)
  // 閉じたあとや次の書き出しを始めたあとに届いた結果を捨てるため、書き出しごとに数える。
  const generation = useRef(0)
  const opening = useAsyncAction((error) =>
    t('export.open_folder_failed', { error: isolated(error) }),
  )

  const start = (chat: Chat) => {
    const current = ++generation.current
    opening.clear()
    setState({ status: 'running' })
    exportChatMarkdown(chat).then(
      (outcome) => {
        if (current !== generation.current) return
        setState(outcome.status === 'cancelled' ? null : { status: 'done', outcome })
      },
      (e: unknown) => {
        if (current !== generation.current) return
        setState({ status: 'failed', error: failureText(e) })
      },
    )
  }
  const close = () => {
    generation.current++
    setState(null)
  }

  const dialog = state && (
    <Dialog title={t('export.button')} onClose={close}>
      {state.status === 'running' && <p>{t('export.exporting')}</p>}
      {state.status === 'done' && <ExportOutcomeText outcome={state.outcome} />}
      {state.status === 'failed' && (
        <p className="error">{t('export.failed', { error: isolated(state.error) })}</p>
      )}
      {opening.error && <p className="error">{opening.error}</p>}
      <div className="button-row dialog-actions">
        {state.status === 'done' && state.outcome.status === 'written' && (
          <button type="button" onClick={() => void opening.run(openExportFolder)}>
            {t('export.open_folder')}
          </button>
        )}
        <button type="button" onClick={close}>
          {t('common.close')}
        </button>
      </div>
    </Dialog>
  )
  return { start, dialog }
}
