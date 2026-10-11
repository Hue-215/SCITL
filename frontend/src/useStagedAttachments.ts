import { useCallback, useRef, useState } from 'react'
import { discardStagedAttachment, failureText, stageReceivedFile } from './api'
import { formatBytes, isolated, t } from './i18n'
import type { AttachmentKind, ReceivedFiles, Rejection } from './types'

/** 送信前の添付1件。 */
export type StagedItem = { key: string; name: string } & (
  | { state: 'staging' }
  | { state: 'staged'; token: string; kind: AttachmentKind; size: number }
  | { state: 'rejected'; message: string }
)

/** 送信のために取り出した添付。送信が失敗したら[`StagedAttachments.restore`]で戻す。 */
export interface TakenAttachments {
  items: StagedItem[]
  tokens: string[]
  names: string[]
}

export interface StagedAttachments {
  items: StagedItem[]
  /**
   * Rust側が受け取ったファイル(落とした・選んだ・貼り付けた)を、受け取りの番号と位置で指して
   * 預ける。受け付けるか(種別・大きさ・1つの発言に付けられる数)はRust側が決める。
   */
  addReceived: (files: ReceivedFiles) => void
  remove: (key: string) => void
  /** 送る添付を取り出し、一覧を空にする。受け付けなかったものも一緒に消える。 */
  take: () => TakenAttachments
  /**
   * 送信のコマンドが失敗したときに、取り出した添付を一覧へ戻す。Rust側も発言を書けなければ
   * 預かりに戻す(`orchestration::run_turn`)ので、同じトークンで送り直せる。
   */
  restore: (taken: TakenAttachments) => void
  /** まだ判定を待っているものがある(送信できない)。 */
  busy: boolean
  /** 送れるものがある。 */
  ready: boolean
  /**
   * 1つの発言に付けられる数を超えて断った添付があれば、その数の上限。チップを積まずにダイアログで
   * 知らせる(超えた分を1件ずつチップにすると、連続して貼り付けたときに入力欄を押し上げるため)。
   */
  overLimit: number | null
  /** `overLimit`のダイアログを閉じる。 */
  dismissOverLimit: () => void
}

/** 入力欄の送信前の添付。受け取ったファイルはRust側で判定させて預け、トークンで持つ。 */
export function useStagedAttachments(): StagedAttachments {
  const [items, setItems] = useState<StagedItem[]>([])
  const [overLimit, setOverLimit] = useState<number | null>(null)
  // 判定を待つ間に取り消した添付。戻ってきたトークンをその場で破棄する。
  const withdrawn = useRef(new Set<string>())
  const nextKey = useRef(0)

  const settle = (key: string, next: StagedItem) => {
    setItems((prev) => prev.map((item) => (item.key === key ? next : item)))
  }

  const addReceived = ({ batch_id, names }: ReceivedFiles) => {
    const added: StagedItem[] = names.map((name, index) => {
      const key = String(nextKey.current++)
      stageReceivedFile(batch_id, index).then(
        (outcome) => {
          if (withdrawn.current.delete(key)) {
            if (outcome.status === 'staged') void discardStagedAttachment(outcome.token)
            return
          }
          // 上限に収まる分は足したまま、超えた分はチップから外してダイアログで知らせる。
          if (outcome.status === 'rejected' && outcome.reason === 'too_many') {
            setItems((prev) => prev.filter((item) => item.key !== key))
            setOverLimit(outcome.limit)
            return
          }
          settle(
            key,
            outcome.status === 'staged'
              ? {
                  key,
                  name,
                  state: 'staged',
                  token: outcome.token,
                  kind: outcome.kind,
                  size: outcome.size_bytes,
                }
              : { key, name, state: 'rejected', message: rejectionText(outcome) },
          )
        },
        (e) => {
          if (withdrawn.current.delete(key)) return
          settle(key, {
            key,
            name,
            state: 'rejected',
            message: t('attachment.load_failed', { error: isolated(failureText(e)) }),
          })
        },
      )
      return { key, name, state: 'staging' }
    })
    setItems((prev) => [...prev, ...added])
  }

  const remove = (key: string) => {
    const item = items.find((i) => i.key === key)
    if (!item) return
    if (item.state === 'staging') withdrawn.current.add(key)
    // 預かりから外せなくても、送らなければ再起動で消えるだけなので、失敗は画面に出さない。
    if (item.state === 'staged') void discardStagedAttachment(item.token).catch(() => undefined)
    setItems((prev) => prev.filter((i) => i.key !== key))
  }

  const take = useCallback((): TakenAttachments => {
    const staged = items.filter((i) => i.state === 'staged')
    setItems([])
    return {
      items: staged,
      tokens: staged.map((i) => i.token),
      names: staged.map((i) => i.name),
    }
  }, [items])

  // 失敗を待つ間に選んだ添付があれば、その前に戻す(選んだ順を保つ)。
  const restore = useCallback((taken: TakenAttachments) => {
    setItems((prev) => [...taken.items, ...prev])
  }, [])

  const dismissOverLimit = useCallback(() => setOverLimit(null), [])

  return {
    items,
    addReceived,
    remove,
    take,
    restore,
    busy: items.some((i) => i.state === 'staging'),
    ready: items.some((i) => i.state === 'staged'),
    overLimit,
    dismissOverLimit,
  }
}

/** 預けなかった理由の文言(チップに出すもの。数の上限はダイアログで知らせる)。 */
function rejectionText(outcome: Exclude<Rejection, { reason: 'too_many' }>): string {
  switch (outcome.reason) {
    case 'too_large':
      return t('attachment.too_large', { limit: formatBytes(outcome.limit_bytes) })
    case 'not_a_file':
      return t('attachment.not_a_file')
    case 'unsupported':
      return t('attachment.unsupported')
    case 'image_unreadable':
      return t('attachment.image_unreadable')
  }
}
