import { useCallback, useEffect, useRef, useState } from 'react'
import {
  discardStagedAttachment,
  failureText,
  getAttachmentLimits,
  stageAttachment,
  stageDroppedFile,
} from './api'
import { formatBytes, isolated, t } from './i18n'
import type { AttachmentKind, DropNotice, PickingLimits, StageOutcome } from './types'

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
  /** 選んだ・貼り付けたファイルを、中身を渡して預ける。 */
  add: (files: File[]) => void
  /** 窓に落としたファイルを、Rust側が受け取ったドロップの番号と位置で指して預ける。 */
  addDropped: (notice: DropNotice) => void
  remove: (key: string) => void
  /** 送る添付を取り出し、一覧を空にする。受け付けなかったものも一緒に消える。 */
  take: () => TakenAttachments
  /**
   * 送信のコマンドが失敗したときに、取り出した添付を一覧へ戻す。Rust側も発言を書けなければ
   * 預かりに戻す(`orchestration::run_turn`)ので、同じトークンで送り直せる。
   */
  restore: (taken: TakenAttachments) => void
  /** 添付を選べる(上限を受け取ってから。受け取る前は大きさ・数の確かめができない)。 */
  canAdd: boolean
  /** まだ判定を待っているものがある(送信できない)。 */
  busy: boolean
  /** 送れるものがある。 */
  ready: boolean
}

/** 預ける元のファイル1つ。`size`は預ける前に分かるときだけ持つ(落としたファイルは分からない)。 */
interface Source {
  name: string
  size: number | null
  stage: () => Promise<StageOutcome>
}

/** 入力欄の送信前の添付。選んだファイルはRust側で判定させて預け、トークンで持つ。 */
export function useStagedAttachments(): StagedAttachments {
  const [items, setItems] = useState<StagedItem[]>([])
  const [limits, setLimits] = useState<PickingLimits | null>(null)
  // 判定を待つ間に取り消した添付。戻ってきたトークンをその場で破棄する。
  const withdrawn = useRef(new Set<string>())
  const nextKey = useRef(0)

  useEffect(() => {
    getAttachmentLimits().then(setLimits, () => undefined)
  }, [])

  const settle = (key: string, next: StagedItem) => {
    setItems((prev) => prev.map((item) => (item.key === key ? next : item)))
  }

  const addSources = (sources: Source[]) => {
    const counted = items.filter((i) => i.state !== 'rejected').length
    const added: StagedItem[] = sources.map(({ name, size, stage }, i) => {
      const key = String(nextKey.current++)
      if (limits && counted + i >= limits.per_message) {
        return {
          key,
          name,
          state: 'rejected',
          message: t('attachment.too_many', { count: limits.per_message }),
        }
      }
      // どの種別でも受け付けない大きさなら、中身を読む前に弾く(種別ごとの上限はRust側が見る)。
      if (limits && size !== null && size > limits.largest_bytes) {
        return {
          key,
          name,
          state: 'rejected',
          message: t('attachment.too_large', { limit: formatBytes(limits.largest_bytes) }),
        }
      }
      stage().then(
        (outcome) => {
          if (withdrawn.current.delete(key)) {
            if (outcome.status === 'staged') void discardStagedAttachment(outcome.token)
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

  const add = (files: File[]) =>
    addSources(
      files.map((file) => ({
        name: file.name,
        size: file.size,
        stage: () => stageAttachment(file),
      })),
    )

  const addDropped = (notice: DropNotice) =>
    addSources(
      notice.names.map((name, index) => ({
        name,
        size: null,
        stage: () => stageDroppedFile(notice.drop_id, index),
      })),
    )

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

  return {
    items,
    add,
    addDropped,
    remove,
    take,
    restore,
    canAdd: limits !== null,
    busy: items.some((i) => i.state === 'staging'),
    ready: items.some((i) => i.state === 'staged'),
  }
}

/** 預けなかった理由の文言。 */
function rejectionText(outcome: StageOutcome & { status: 'rejected' }): string {
  switch (outcome.reason) {
    case 'too_large':
      return t('attachment.too_large', { limit: formatBytes(outcome.limit_bytes) })
    case 'not_a_file':
      return t('attachment.not_a_file')
  }
}
