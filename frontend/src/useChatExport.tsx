import { useState } from 'react'
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
// ダイアログに出す。保存画面を取りやめたらダイアログを閉じる。書き出しは止められないので、終わるまで
// ダイアログを閉じさせない(書きかけのファイルが残りうること等の結果を必ず見せるため。`export.md`)。
export function useChatExport() {
  const [state, setState] = useState<ChatExport | null>(null)
  const opening = useAsyncAction((error) =>
    t('export.open_folder_failed', { error: isolated(error) }),
  )

  const running = state?.status === 'running'
  // ダイアログに押せる部品が無い間は、Tabで背後の︙へ抜けられるので、ここでも重ねて始めない。
  const start = (chat: Chat) => {
    if (running) return
    opening.clear()
    setState({ status: 'running' })
    exportChatMarkdown(chat).then(
      (outcome) => {
        setState(outcome.status === 'cancelled' ? null : { status: 'done', outcome })
      },
      (e: unknown) => {
        setState({ status: 'failed', error: failureText(e) })
      },
    )
  }
  const close = () => {
    if (!running) setState(null)
  }

  const dialog = state && (
    <Dialog title={t('export.button')} onClose={close}>
      {state.status === 'running' && <p>{t('export.exporting')}</p>}
      {state.status === 'done' && <ExportOutcomeText outcome={state.outcome} />}
      {state.status === 'failed' && (
        <p className="error">{t('export.failed', { error: isolated(state.error) })}</p>
      )}
      {opening.error && <p className="error">{opening.error}</p>}
      {!running && (
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
      )}
    </Dialog>
  )
  return { start, dialog }
}
