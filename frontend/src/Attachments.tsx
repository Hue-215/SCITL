import { useEffect, useState } from 'react'
import { failureText, readImageAttachment, readTextAttachment, revealAttachment } from './api'
import Chip from './Chip'
import Dialog from './Dialog'
import { formatBytes, t } from './i18n'
import type { AttachmentDelivery, AttachmentKind, AttachmentView } from './types'
import type { StagedAttachments } from './useStagedAttachments'

// 添付ファイル(Issue #21)。種別の判定・大きさの上限・モデルへの渡し方はRust側が決め、
// ここは結果を描くだけ(architecture.md 12節)。

// 拡大表示の枠の幅。確認ダイアログより広く取る(Dialogの`width`)。ここでしか使わない値
// なのでトークン化しない。
const PREVIEW_DIALOG_WIDTH = 'min(48rem, 100%)'

/** 発言に付いた添付。押すと、画像は拡大、テキストは全文、その他は入っているフォルダを開く。 */
export function MessageAttachments({ attachments }: { attachments: AttachmentView[] }) {
  if (attachments.length === 0) return null
  return (
    <div className="chip-list">
      {attachments.map((a) => (
        <AttachmentChip key={a.id} attachment={a} />
      ))}
    </div>
  )
}

function AttachmentChip({ attachment }: { attachment: AttachmentView }) {
  const size = formatBytes(attachment.size_bytes)
  switch (attachment.kind) {
    case 'image':
      return <ImageChip attachment={attachment} size={size} />
    case 'text':
      return <TextChip attachment={attachment} size={size} />
    case 'other':
      return <OtherChip attachment={attachment} size={size} />
  }
}

function ImageChip({ attachment, size }: { attachment: AttachmentView; size: string }) {
  const [url, setUrl] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [open, setOpen] = useState(false)

  useEffect(() => {
    let alive = true
    readImageAttachment(attachment.id).then(
      (loaded) => alive && setUrl(loaded),
      (e) => alive && setError(failureText(e)),
    )
    return () => {
      alive = false
    }
  }, [attachment.id])

  return (
    <>
      <Chip
        label={attachment.original_name}
        detail={size}
        tone={error ? 'error' : 'normal'}
        title={error ? t('attachment.load_failed', { error }) : undefined}
        leading={url && <img className="chip-thumbnail" src={url} alt="" />}
        onOpen={url ? () => setOpen(true) : undefined}
      />
      {open && url && (
        <Dialog
          title={attachment.original_name}
          onClose={() => setOpen(false)}
          width={PREVIEW_DIALOG_WIDTH}
        >
          <img className="attachment-preview-image" src={url} alt={attachment.original_name} />
        </Dialog>
      )}
    </>
  )
}

function TextChip({ attachment, size }: { attachment: AttachmentView; size: string }) {
  const [open, setOpen] = useState(false)
  return (
    <>
      <Chip label={attachment.original_name} detail={size} onOpen={() => setOpen(true)} />
      {open && <TextDialog attachment={attachment} onClose={() => setOpen(false)} />}
    </>
  )
}

function TextDialog({ attachment, onClose }: { attachment: AttachmentView; onClose: () => void }) {
  const [text, setText] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let alive = true
    readTextAttachment(attachment.id).then(
      (loaded) => alive && setText(loaded),
      (e) => alive && setError(failureText(e)),
    )
    return () => {
      alive = false
    }
  }, [attachment.id])

  return (
    <Dialog title={attachment.original_name} onClose={onClose} width={PREVIEW_DIALOG_WIDTH}>
      {error ? (
        <p className="error">{t('attachment.load_failed', { error })}</p>
      ) : (
        // 利用者やファイルの書き手が書いたものなので、Markdownとして解釈せずそのまま出す。
        <pre className="attachment-preview-text">{text ?? t('attachment.loading')}</pre>
      )}
    </Dialog>
  )
}

function OtherChip({ attachment, size }: { attachment: AttachmentView; size: string }) {
  const [error, setError] = useState<string | null>(null)
  return (
    <Chip
      label={attachment.original_name}
      detail={size}
      tone={error ? 'error' : 'normal'}
      title={error ? t('attachment.load_failed', { error }) : t('attachment.reveal_tooltip')}
      onOpen={() => {
        setError(null)
        revealAttachment(attachment.id).catch((e) => setError(failureText(e)))
      }}
    />
  )
}

/** 入力欄の上に並べる送信前の添付。モデルが中身を受け取れない種別には警告を出す(送信は止めない)。 */
export function StagedAttachmentChips({
  staged,
  deliveries,
  disabled,
}: {
  staged: StagedAttachments
  // 選んでいるモデルの、種別ごとの渡し方。モデルが未選択なら警告は出さない。
  deliveries: Record<AttachmentKind, AttachmentDelivery> | null
  disabled: boolean
}) {
  if (staged.items.length === 0) return null
  return (
    <div className="chip-list chat-compose-attachments">
      {staged.items.map((item) => {
        const common = {
          label: item.name,
          onRemove: () => staged.remove(item.key),
          removeLabel: t('attachment.remove', { name: item.name }),
          disabled,
        }
        switch (item.state) {
          case 'staging':
            return <Chip key={item.key} {...common} detail={t('attachment.loading')} />
          case 'rejected':
            return (
              <Chip
                key={item.key}
                {...common}
                tone="error"
                title={item.message}
                leading={<span className="chip-mark">⚠</span>}
              />
            )
          case 'staged': {
            const warning =
              deliveries?.[item.kind] !== 'name_only'
                ? null
                : item.kind === 'image'
                  ? t('attachment.image_not_sent')
                  : t('attachment.content_not_sent')
            return (
              <Chip
                key={item.key}
                {...common}
                detail={formatBytes(item.size)}
                tone={warning ? 'warning' : 'normal'}
                title={warning ?? undefined}
                leading={warning && <span className="chip-mark">⚠</span>}
              />
            )
          }
        }
      })}
    </div>
  )
}

/** 楽観表示のユーザー発言に出す、送った添付の名前(確定するまで開けない)。 */
export function PendingAttachments({ names }: { names: string[] }) {
  if (names.length === 0) return null
  return (
    <div className="chip-list">
      {names.map((name, i) => (
        <Chip key={i} label={name} />
      ))}
    </div>
  )
}
