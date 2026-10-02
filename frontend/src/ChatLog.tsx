import type { RefObject, UIEventHandler } from 'react'
import { MessageAttachments, PendingAttachments } from './Attachments'
import { ConfirmButton } from './Dialog'
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
// 削除はその発言より後ろもまとめて消すので、確認を挟む。
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
      <ConfirmButton
        label={t('common.delete')}
        confirmTitle={t('chat.delete_dialog_title')}
        confirmMessage={t('chat.delete_dialog_message')}
        confirmLabel={t('common.delete')}
        onConfirm={onDelete}
        disabled={disabled}
      />
    </div>
  )
}

// 返信の無い会話の末尾に出す、応答を生成する操作。止めたターンが会話の最後にあるときも同じ形で出す。
function GenerateReplyButton({ onClick, disabled }: { onClick: () => void; disabled: boolean }) {
  return (
    <button type="button" disabled={disabled} onClick={onClick}>
      {t('chat.generate_reply_button')}
    </button>
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
  // 会話が返信の無いまま終わっているときだけ渡す。会話の末尾に応答を生成する操作を出す。
  onGenerateReply: (() => void) | null
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
  onGenerateReply,
}: ChatLogProps) {
  const items = groupMessages(messages)
  // 会話の最後のやり取り(ユーザー発言かターン)の位置。後ろに並ぶ操作の記録は数えない。送信中は
  // 楽観表示のユーザー発言が後ろに来るので、保存済みの項目はどれも最後ではない。
  const lastExchange =
    pending.length > 0
      ? items.length
      : items.findLastIndex((i) => i.kind === 'turn' || i.message.role === 'user')
  // 応答を生成中に答えているユーザー発言(最後のユーザー発言)。画像を渡したかがまだ決まって
  // いないので、画像に渡していない印を出さない。送信中は答えている発言がまだ楽観表示にしか無い
  // (保存済みの最後のユーザー発言は、前の発言)。
  const answering =
    pending.some((entry) => entry.role === 'pending') &&
    !pending.some((entry) => entry.role === 'user')
      ? messages.findLast((m) => m.role === 'user' && m.kind === 'normal')?.id
      : undefined
  const undeliveredOf = (message: MessageView) =>
    message.id === answering
      ? message.undelivered_attachments.filter(
          (id) => message.attachments.find((a) => a.id === id)?.kind !== 'image',
        )
      : message.undelivered_attachments
  return (
    <ul className="chat-log" ref={logRef} onScroll={onScroll}>
      {items.map((item, index) => {
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

          // 編集・削除。`plain`項目は常にユーザー発言なので、編集はここでのみ起こりうる。
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
                <MessageAttachments
                  attachments={message.attachments}
                  undelivered={undeliveredOf(message)}
                />
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
              <MessageAttachments
                attachments={message.attachments}
                undelivered={undeliveredOf(message)}
              />
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

        // 応答生成1ターン分。思考・ツール呼び出しを発生順の折りたたみで見せ、返信(最終行。
        // エラー発言を含む)を吹き出しとして表示する。再試行・削除の対象はこの最終行。
        const finalMessage = finalEntryOf(item.entries)
        const canRetryOrDelete =
          finalMessage.kind === 'normal' &&
          (finalMessage.role === 'assistant' || finalMessage.role === 'error')
        // ユーザーが止めたターンは失敗として見せない。会話の最後なら返信の無い会話と同じく
        // 応答を生成する操作(中身は作り直し)を、続けて発言したあとなら止めたことだけを出す。
        if (finalMessage.role === 'error' && finalMessage.error_kind === 'stopped') {
          return (
            <li key={`turn-${item.turnId}`} className="turn-group">
              <ThinkingTools items={buildThoughtItems(item.entries)} />
              {finalMessage.partial_reply && (
                <div className="entry entry-assistant">
                  <EntryBody role="assistant" content={finalMessage.partial_reply} />
                </div>
              )}
              {index === lastExchange ? (
                <div className="button-row">
                  <GenerateReplyButton
                    onClick={() => onRetry(finalMessage.id)}
                    disabled={disableActions}
                  />
                </div>
              ) : (
                <p className="entry-stopped">{t('chat.stopped_note')}</p>
              )}
            </li>
          )
        }
        return (
          <li key={`turn-${item.turnId}`} className="turn-group">
            <ThinkingTools items={buildThoughtItems(item.entries)} />
            {/* 失敗したターンで受け取り終えた本文。生成中に見えていたものを返信と同じ形で残す */}
            {finalMessage.partial_reply && (
              <div className="entry entry-assistant">
                <EntryBody role="assistant" content={finalMessage.partial_reply} />
              </div>
            )}
            <div className={`entry entry-${finalMessage.role}`}>
              <EntryBody
                role={finalMessage.role}
                content={finalMessage.content}
                errorKind={finalMessage.error_kind}
              />
              {/* プロバイダーが書いた文字列のため、Markdown描画の対象にせず
                  プレーンテキストのまま出す */}
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
          // 応答待ちの間の途中経過を、保存済みのターンと同じ形で出す。完了したら読み直した
          // ターンに置き換わる。
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
      {/* コマンド自体の失敗。保存されたエラー発言と同じ見た目にする */}
      {failure && (
        <li className="entry entry-error">
          <span className="entry-content">{failure}</span>
        </li>
      )}
      {onGenerateReply && pending.length === 0 && (
        <li className="button-row">
          <GenerateReplyButton onClick={onGenerateReply} disabled={disableActions} />
        </li>
      )}
    </ul>
  )
}
