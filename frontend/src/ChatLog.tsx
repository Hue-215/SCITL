import type { RefObject, UIEventHandler } from 'react'
import { MessageAttachments, PendingAttachments } from './Attachments'
import { formatDateTime, t, turnErrorText } from './i18n'
import Markdown from './Markdown'
import { OperationLine, ThinkingTools } from './ThinkingTools'
import { buildThoughtItems, finalEntryOf, groupMessages, type ThoughtItem } from './thinking'
import type { MessageView, PendingEntry } from './types'

// Markdownとして描画するのはユーザーとモデルが書いた本文だけ。エラー発言と応答待ちの
// 表示はSCITL自身の文言(とプロバイダーが返した文字列)なので、プレーンテキストのまま出す。
// エラー発言は、ターンの中でも外でもここで表示言語の文言に替える。
function EntryBody({
  role,
  content,
  errorKind = null,
}: {
  role: string
  content: string
  errorKind?: string | null
}) {
  if (role === 'user' || role === 'assistant') return <Markdown text={content} />
  const text = role === 'error' ? turnErrorText(errorKind, content) : content
  return <span className="entry-content">{text}</span>
}

// 発言の下の操作ボタン行。ユーザー発言(編集・削除)と返信(再試行・削除)で同じ形。
function EntryActions({
  label,
  onAction,
  onDelete,
  disabled,
}: {
  label: string
  onAction: () => void
  onDelete: () => void
  disabled: boolean
}) {
  return (
    <div className="button-row entry-actions">
      <button type="button" disabled={disabled} onClick={onAction}>
        {label}
      </button>
      <button type="button" disabled={disabled} onClick={onDelete}>
        {t('common.delete')}
      </button>
    </div>
  )
}

// 編集中のユーザー発言。編集できるのは1件ずつ。
export interface EntryEditing {
  id: number | null
  draft: string
  setDraft: (draft: string) => void
  start: (message: MessageView) => void
  cancel: () => void
  submit: (message: MessageView) => void
}

interface ChatLogProps {
  logRef: RefObject<HTMLUListElement | null>
  onScroll: UIEventHandler<HTMLUListElement>
  messages: MessageView[]
  // 実行中のコマンドの楽観表示・途中経過・コマンド自体の失敗(`useChatRequests`)。
  pending: PendingEntry[]
  live: ThoughtItem[]
  failure: string | null
  // 応答待ちの会話では、編集・再試行・削除を不可にする。
  disableActions: boolean
  editing: EntryEditing
  onRetry: (messageId: number) => void
  onRemove: (messageId: number) => void
}

// 会話欄。保存済みの発言・応答待ちの表示・コマンドの失敗を並べる。
export default function ChatLog({
  logRef,
  onScroll,
  messages,
  pending,
  live,
  failure,
  disableActions,
  editing,
  onRetry,
  onRemove,
}: ChatLogProps) {
  return (
    <ul className="chat-log" ref={logRef} onScroll={onScroll}>
      {groupMessages(messages).map((item) => {
        if (item.kind === 'plain') {
          const message = item.message
          // 応答生成以外の経路(画面・MCP等)での操作の記録は「思考・ツール」の
          // 折りたたみに含めず、独立した1行として表示する。
          if (message.kind === 'tool_execution') {
            return (
              <li key={message.id} className="entry entry-tool">
                <OperationLine message={message} />
                <time className="entry-time">{formatDateTime(message.created_at)}</time>
              </li>
            )
          }

          // 編集・削除。対象はツール実行記録を除く通常発言のみ
          // (data-model.md「ツール実行記録は通常発言の編集・削除・再試行の対象に
          // 含めない」)。`plain`項目は常にユーザー発言のため、編集はここでのみ
          // 起こりうる。編集と削除は対象が同じ。
          const canEditOrDelete = message.role === 'user'

          if (editing.id === message.id) {
            return (
              <li key={message.id} className={`entry entry-${message.role}`}>
                <textarea
                  className="entry-edit-textarea"
                  value={editing.draft}
                  onChange={(e) => editing.setDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
                      e.preventDefault()
                      editing.submit(message)
                    } else if (e.key === 'Escape') {
                      editing.cancel()
                    }
                  }}
                  autoFocus
                />
                <MessageAttachments attachments={message.attachments} />
                <div className="button-row entry-actions">
                  <button type="button" onClick={editing.cancel}>
                    {t('common.cancel')}
                  </button>
                  <button type="button" onClick={() => editing.submit(message)}>
                    {t('chat.send_button')}
                  </button>
                </div>
              </li>
            )
          }

          return (
            <li key={message.id} className={`entry entry-${message.role}`}>
              {/* 添付だけの発言は本文が空になる */}
              {message.content && (
                <EntryBody
                  role={message.role}
                  content={message.content}
                  errorKind={message.error_kind}
                />
              )}
              <MessageAttachments attachments={message.attachments} />
              <time className="entry-time">{formatDateTime(message.created_at)}</time>
              {canEditOrDelete && (
                <EntryActions
                  label={t('chat.edit_button')}
                  onAction={() => editing.start(message)}
                  onDelete={() => onRemove(message.id)}
                  disabled={disableActions}
                />
              )}
            </li>
          )
        }

        // SCITL自身の応答生成1ターン分。思考・内部ツール呼び出しを発生順の折りたたみで
        // 見せたうえで、実際の返信(最終行)を通常の吹き出しとして表示する。
        // 再試行・削除の対象は、この最終行の通常発言のみ。
        // 再試行と削除は対象が同じ。
        // 失敗したターンの返信(エラー発言)も含める。
        const finalMessage = finalEntryOf(item.entries)
        const canRetryOrDelete =
          finalMessage.kind === 'normal' &&
          (finalMessage.role === 'assistant' || finalMessage.role === 'error')
        return (
          <li key={`turn-${item.turnId}`} className="turn-group">
            <ThinkingTools items={buildThoughtItems(item.entries)} />
            <div className={`entry entry-${finalMessage.role}`}>
              <EntryBody
                role={finalMessage.role}
                content={finalMessage.content}
                errorKind={finalMessage.error_kind}
              />
              {/* プロバイダーが書いた文字列のため、Markdown描画(#39)の対象にせず
                  プレーンテキストのまま出す(Issue #159) */}
              {finalMessage.error_detail && (
                <details className="entry-error-detail">
                  <summary>{t('chat.error_detail_summary')}</summary>
                  <pre>{finalMessage.error_detail}</pre>
                </details>
              )}
              <time className="entry-time">{formatDateTime(finalMessage.created_at)}</time>
              {canRetryOrDelete && (
                <EntryActions
                  label={t('chat.retry_button')}
                  onAction={() => onRetry(finalMessage.id)}
                  onDelete={() => onRemove(finalMessage.id)}
                  disabled={disableActions}
                />
              )}
            </div>
          </li>
        )
      })}
      {pending.map((entry, i) =>
        entry.role === 'pending' ? (
          // 応答待ちの間の途中経過を、保存済みのターンと同じ形で出す。
          // 完了したら読み直したターンに置き換わる。
          <li key={`pending-${i}`} className="turn-group">
            <ThinkingTools items={live} />
            <div className="entry entry-pending">
              <EntryBody role={entry.role} content={entry.content} />
            </div>
          </li>
        ) : (
          <li key={`pending-${i}`} className={`entry entry-${entry.role}`}>
            {entry.content && <EntryBody role={entry.role} content={entry.content} />}
            <PendingAttachments names={entry.attachmentNames ?? []} />
          </li>
        ),
      )}
      {/* コマンド自体の失敗。保存されたエラー発言と同じ見た目にする(Issue #152) */}
      {failure && (
        <li className="entry entry-error">
          <span className="entry-content">{failure}</span>
        </li>
      )}
    </ul>
  )
}
