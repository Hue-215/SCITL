import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from 'react'
import { watchDroppedFiles } from './api'
import { StagedAttachmentChips } from './Attachments'
import { PASTED_IMAGES_NEED_READING, readClipboardImages } from './clipboard'
import ChatModelBar from './ChatModelBar'
import { t } from './i18n'
import { isCommitEnter } from './keyboard'
import type { AttachmentDeliveries, DropNotice } from './types'
import {
  type StagedAttachments,
  type TakenAttachments,
  useStagedAttachments,
} from './useStagedAttachments'

// チャットの入力欄。本文・送信前の添付・送信できるかの判定をここに閉じ、1文字ごとの再描画を
// 入力欄の中に留める(会話欄まで描き直さない)。

interface ComposeState {
  draft: string
  setDraft: (draft: string) => void
  staged: StagedAttachments
  // 窓に落としたファイルの受け取り先を差し替える。入力欄が出ていて、添付を足せるときだけ置く。
  setDropTarget: (target: ((notice: DropNotice) => void) | null) => void
}

const ComposeContext = createContext<ComposeState | null>(null)

/**
 * 入力欄の本文と送信前の添付を持つ。会話を切り替えても、設定画面を開いて入力欄が外れても
 * 書きかけを残すため、画面の切り替えより上に置く。中身は`ChatCompose`だけが読むので、
 * 状態が変わっても描き直されるのは入力欄だけになる(`children`は外から渡された同じ要素の
 * まま)。
 *
 * 窓に落としたファイルもここで受ける。パスはOSからRust側へ直接届き、画面には名前だけが知らされる
 * (`watchDroppedFiles`)。窓のどこに落としても入力欄の添付に加え、入力欄が出ていない・添付を
 * 足せないときは受け付けない(読ませないまま、次のドロップで捨てられる)。
 */
export function ComposeProvider({ children }: { children: ReactNode }) {
  const [draft, setDraft] = useState('')
  const staged = useStagedAttachments()
  const dropTarget = useRef<((notice: DropNotice) => void) | null>(null)
  const setDropTarget = useCallback((target: ((notice: DropNotice) => void) | null) => {
    dropTarget.current = target
  }, [])

  useEffect(() => {
    // 送り先は1つで、渡し直すと置き換わる(StrictModeで2回渡しても、後のものだけが残る)。
    watchDroppedFiles((notice) => dropTarget.current?.(notice)).catch(() => undefined)
  }, [])

  return (
    <ComposeContext value={{ draft, setDraft, staged, setDropTarget }}>{children}</ComposeContext>
  )
}

/** 送信する発言。送信のコマンドが失敗したら`restore`で添付を入力欄へ戻す。 */
export interface ComposedMessage {
  text: string
  attachments: TakenAttachments
  restore: () => void
}

export default function ChatCompose({
  disabled,
  generating,
  stopping,
  onSend,
  onStop,
  onError,
  onModelChanged,
}: {
  // 表示中の会話が応答待ち。送信も添付の出し入れもできない。
  disabled: boolean
  // 応答を生成中。送信ボタンの位置に停止ボタンを出す。
  generating: boolean
  // 止める指示を出したあと。停止ボタンを押せなくする。
  stopping: boolean
  onSend: (message: ComposedMessage) => void
  onStop: () => void
  onError: (message: string) => void
  // モデルの選択を変えられたとき。
  onModelChanged: () => void
}) {
  const compose = useContext(ComposeContext)
  if (!compose) throw new Error('ChatCompose needs ComposeProvider')
  const { draft, setDraft, staged, setDropTarget } = compose
  // 添付を足せるか。選ぶ・落とす・貼り付けるのどれにも同じ条件を使う。
  const canAdd = !disabled && staged.canAdd
  const fileInputRef = useRef<HTMLInputElement>(null)
  // 選んでいるモデルが添付を種別ごとにどう受け取るか。警告の判断はRust側が済ませてある。
  const [deliveries, setDeliveries] = useState<AttachmentDeliveries | null>(null)

  // 落としたファイルの受け取り先を、描くたびに今の`staged`へ向け直す(`addDropped`は描くたびに
  // 変わる)。応答待ちになった描画のすぐ後から受け付けないよう、画面に出す前に差し替える。
  // 貼り付けの画像を読み直したあとの受け取り先。読み終えたときの入力欄の状態で受ける。
  const pasteTarget = useRef<((files: File[]) => void) | null>(null)
  useLayoutEffect(() => {
    pasteTarget.current = canAdd ? staged.add : null
    setDropTarget(canAdd ? staged.addDropped : null)
    return () => {
      pasteTarget.current = null
      setDropTarget(null)
    }
  })

  // 本文が空でも、添付があれば送れる。判定を待っている添付があるうちは送らない。
  const canSend = !disabled && !staged.busy && (draft.trim() !== '' || staged.ready)

  const send = () => {
    if (!canSend) return
    const attachments = staged.take()
    setDraft('')
    onSend({ text: draft.trim(), attachments, restore: () => staged.restore(attachments) })
  }

  return (
    <>
      <StagedAttachmentChips staged={staged} deliveries={deliveries} disabled={disabled} />

      <div className="chat-compose-area">
        <form
          className="chat-compose"
          onSubmit={(e) => {
            e.preventDefault()
            send()
          }}
        >
          <input
            ref={fileInputRef}
            type="file"
            multiple
            hidden
            onChange={(e) => {
              staged.add(Array.from(e.target.files ?? []))
              // 同じファイルをもう一度選んでも変更として届くように空へ戻す。
              e.target.value = ''
            }}
          />
          <button
            type="button"
            disabled={!canAdd}
            title={t('attachment.add_tooltip')}
            onClick={() => fileInputRef.current?.click()}
          >
            {t('attachment.add_button')}
          </button>
          <textarea
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onPaste={(e) => {
              // 文字も載っていれば文字として貼る。表計算ソフト等は、コピーしたセルの文字と
              // 一緒に、その見た目の画像も載せるため。
              const text = e.clipboardData.getData('text/plain')
              if (text.trim() !== '') return
              const files = Array.from(e.clipboardData.files)
              if (files.length > 0) {
                e.preventDefault()
                if (canAdd) staged.add(files)
                return
              }
              // 文字(空白だけを除く)もファイルも無い。WebKitGTKは画像を`clipboardData`に
              // 入れないので読み直す。空白だけの文字は、画像があるか分からないので既定どおり貼る。
              if (!PASTED_IMAGES_NEED_READING || !canAdd) return
              void readClipboardImages().then((images) => {
                if (images.length > 0) pasteTarget.current?.(images)
              })
            }}
            onKeyDown={(e) => {
              if (isCommitEnter(e) && !e.shiftKey) {
                e.preventDefault()
                send()
              }
            }}
            disabled={disabled}
            placeholder={t('chat.input_hint')}
          />
          {generating ? (
            // 送信ボタンとは別の要素にする(同じ要素だと、送信を押したフォーカスが残り、
            // 応答待ちの間のEnterで止めてしまう)。
            <button
              key="stop"
              type="button"
              className="primary"
              disabled={stopping}
              onClick={(e) => {
                // 送信をダブルクリックした2回目が、入れ替わった停止ボタンに当たっても止めない。
                if (e.detail > 1) return
                onStop()
              }}
            >
              {t('chat.stop_button')}
            </button>
          ) : (
            <button key="send" type="submit" className="primary" disabled={!canSend}>
              {t('chat.send_button')}
            </button>
          )}
        </form>

        <ChatModelBar onError={onError} onChanged={onModelChanged} onDeliveries={setDeliveries} />
      </div>
    </>
  )
}
