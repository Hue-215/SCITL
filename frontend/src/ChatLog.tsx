import { Fragment } from 'react'
import type { ReactNode, RefObject, UIEventHandler, WheelEventHandler } from 'react'
import { MessageAttachments, PendingAttachments } from './Attachments'
import CodeBlock from './CodeBlock'
import { ConfirmButton } from './Dialog'
import { formatDateTime, isolated, t, turnErrorText } from './i18n'
import Markdown from './Markdown'
import { OperationLine, ThinkingTools } from './ThinkingTools'
import {
  buildTurnSegments,
  finalEntryOf,
  groupMessages,
  type LiveTurn,
  type TurnSegment,
} from './thinking'
import type { MessageView, PendingEntry } from './types'

// Markdownとして描画するのはユーザーとモデルが書いた本文だけ。エラー発言はSCITL自身の文言
// (とプロバイダーが返した文字列)なので、プレーンテキストのまま出す。
// エラー発言は、ターンの中でも外でもここで表示言語の文言に替える。`streaming`は、書いている
// 途中の本文か。
function EntryBody({
  role,
  content,
  errorKind = null,
  streaming = false,
}: {
  role: string
  content: string
  errorKind?: string | null
  streaming?: boolean
}) {
  if (role === 'user' || role === 'assistant') {
    return <Markdown text={content} streaming={streaming} />
  }
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

// ターンの中身の1区切り。本文は返信と同じ吹き出しで出す。`footer`は吹き出しの末尾に足すもの
// (日時と操作)。`streaming`は、応答待ちの間の途中経過か。`showReasoning`は思考を出すか。
function TurnSegmentView({
  segment,
  showReasoning,
  footer = null,
  streaming = false,
}: {
  segment: TurnSegment
  showReasoning: boolean
  footer?: ReactNode
  streaming?: boolean
}) {
  if (segment.kind === 'thoughts') {
    return <ThinkingTools items={segment.items} showReasoning={showReasoning} />
  }
  if (segment.kind === 'truncated') return <p className="entry-truncated">{t('chat.truncated_note')}</p>
  return (
    <div className="entry entry-assistant">
      <EntryBody role="assistant" content={segment.text} streaming={streaming} />
      {segment.truncated && <p className="entry-truncated">{t('chat.truncated_note')}</p>}
      {footer}
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
  onWheel: WheelEventHandler<HTMLUListElement>
  messages: MessageView[]
  // 実行中のコマンドの楽観表示・途中経過・コマンド自体の失敗(`useChatRequests`)。
  pending: PendingEntry[]
  // 応答待ちの間に届いたターンの中身。
  live: LiveTurn
  failure: string | null
  // 思考を出すか(設定「一般」)。
  showReasoning: boolean
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
  onWheel,
  messages,
  pending,
  live,
  failure,
  showReasoning,
  disableActions,
  editing,
  onRetry,
  onRemove,
}: ChatLogProps) {
  const items = groupMessages(messages)
  return (
    <ul className="chat-log" ref={logRef} onScroll={onScroll} onWheel={onWheel}>
      {items.map((item) => {
        if (item.kind === 'plain') {
          const message = item.message
          // 応答生成以外の経路(画面・MCP等)での操作の記録はターンに含めず、経路のラベルを
          // 付けた独立した1行として表示する。
          if (message.kind === 'tool_execution') {
            return (
              <li key={message.id} className="entry entry-tool">
                <OperationLine message={message} />
                <time className="entry-time">{formatDateTime(message.created_at)}</time>
              </li>
            )
          }

          // 履歴を間引いていて、モデルへ送っている範囲がこの発言から始まるなら、その前に印を出す
          // (どの発言からかはRust側が決める)。
          const sentFromHere = message.starts_sent_history && (
            <li className="history-start">{t('chat.history_start_note')}</li>
          )

          // 編集・削除。`plain`項目は常にユーザー発言なので、編集はここでのみ起こりうる。
          const canEditOrDelete = message.role === 'user'

          if (editing.id === message.id) {
            return (
              <Fragment key={message.id}>
                {sentFromHere}
                <li className={`entry entry-${message.role}`}>
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
                    undelivered={message.undelivered_attachments}
                    revealable={message.revealable_attachments}
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
              </Fragment>
            )
          }

          return (
            <Fragment key={message.id}>
              {sentFromHere}
              <li className={`entry entry-${message.role}`}>
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
                  undelivered={message.undelivered_attachments}
                  revealable={message.revealable_attachments}
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
            </Fragment>
          )
        }

        // 応答生成1ターン分。中身(思考・ツールの並びと、ラウンドごとの本文)を起きた順に
        // 並べる。再試行・削除の対象は返信の行(最終行。エラー発言を含む)。
        const finalMessage = finalEntryOf(item.entries)
        const segments = buildTurnSegments(item.entries)
        const canRetryOrDelete =
          finalMessage.kind === 'normal' &&
          (finalMessage.role === 'assistant' || finalMessage.role === 'error')
        const actions = canRetryOrDelete && (
          <EntryActions
            label={t('chat.retry_button')}
            onAction={() => onRetry(finalMessage.id)}
            onDelete={() => onRemove(finalMessage.id)}
            disabled={disableActions}
          />
        )
        const footer = (
          <>
            <time className="entry-time">{formatDateTime(finalMessage.created_at)}</time>
            {actions}
          </>
        )
        // ユーザーが止めたターンは失敗として見せず、止めたことだけを出す。会話の最後なら、入力欄の
        // 「応答を生成」がこのターンを作り直す(`stoppedTurnAtEnd`)。
        if (finalMessage.role === 'error' && finalMessage.error_kind === 'stopped') {
          return (
            <li key={`turn-${item.turnId}`} className="turn-group">
              {segments.map((segment, i) => (
                <TurnSegmentView key={i} segment={segment} showReasoning={showReasoning} />
              ))}
              <p className="entry-stopped">{t('chat.stopped_note')}</p>
            </li>
          )
        }
        // 返信は、最後に来た本文の吹き出しに日時と操作を添える。本文のあとに思考・ツールが続いた
        // (最後のラウンドが本文を書かなかった)ときは、ターンの末尾に添える。失敗したターンの本文
        // (受け取り終えたもの)は生成中に見えていたものを返信と同じ形で残し、日時と操作はエラーに
        // 添える。
        const footerOnText =
          finalMessage.role === 'assistant' && segments.at(-1)?.kind === 'text'
        return (
          <li key={`turn-${item.turnId}`} className="turn-group">
            {segments.map((segment, i) => (
              <TurnSegmentView
                key={i}
                segment={segment}
                showReasoning={showReasoning}
                footer={footerOnText && i === segments.length - 1 ? footer : null}
              />
            ))}
            {finalMessage.role === 'assistant' ? (
              !footerOnText && <div>{footer}</div>
            ) : (
              <div className={`entry entry-${finalMessage.role}`}>
                <EntryBody
                  role={finalMessage.role}
                  content={finalMessage.content}
                  errorKind={finalMessage.error_kind}
                />
                {/* プロバイダーが書いた文字列のため、Markdown描画の対象にせず
                    プレーンテキストのまま出す */}
                {finalMessage.error_detail && (
                  <CodeBlock className="entry-error-detail" label={t('chat.error_detail_label')}>
                    {finalMessage.error_detail}
                  </CodeBlock>
                )}
                {footer}
              </div>
            )}
          </li>
        )
      })}
      {pending.map((entry, i) =>
        entry.role === 'pending' ? (
          // 応答待ちの間の途中経過を、保存済みのターンと同じ形で届いた順に出す。完了したら
          // 読み直したターンに置き換わる。応答を待っていることは入力欄の上に出す(`TypingIndicator`)。
          live.segments.length > 0 && (
            <li key={`pending-${i}`} className="turn-group">
              {live.segments.map((segment, j) => (
                <TurnSegmentView
                  key={j}
                  segment={segment}
                  showReasoning={showReasoning}
                  streaming
                />
              ))}
            </li>
          )
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
    </ul>
  )
}

// モデルの応答を待っている間、入力欄の上に出す「入力中」。`provider`は待っているプロバイダーの
// 名前で、待っていなければ`null`(本文が流れている間・ツールの実行中・止める指示のあと)。
// 行は生成中ずっと置き、文言だけを出し入れする(ラウンドのたびに会話欄の高さが変わらないように)。
export function TypingIndicator({ provider }: { provider: string | null }) {
  return (
    <p className="typing-indicator" role="status">
      {provider !== null && t('chat.typing', { provider: isolated(provider) })}
    </p>
  )
}
