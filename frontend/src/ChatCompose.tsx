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
import {
  discardAllStagedAttachments,
  failureText,
  pasteClipboardImage,
  pickAttachments,
  watchDroppedFiles,
} from './api'
import { StagedAttachmentChips } from './Attachments'
import ChatModelBar from './ChatModelBar'
import { t } from './i18n'
import { isSendEnter } from './keyboard'
import type { AttachmentDeliveries, ReceivedFiles } from './types'
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
  // Rust側が受け取ったファイル(落とした・選んだ・貼り付けた)の受け取り先を差し替える。入力欄が
  // 出ていて、添付を足せるときだけ置く。
  setReceiveTarget: (target: ((files: ReceivedFiles) => void) | null) => void
  // 受け取ったファイルを、そのときの受け取り先へ渡す(置かれていなければ受け付けない)。
  receive: (files: ReceivedFiles) => void
}

const ComposeContext = createContext<ComposeState | null>(null)

/**
 * 入力欄の本文と送信前の添付を持つ。会話を切り替えても、設定画面を開いて入力欄が外れても
 * 書きかけを残すため、画面の切り替えより上に置く。中身は`ChatCompose`だけが読むので、
 * 状態が変わっても描き直されるのは入力欄だけになる(`children`は外から渡された同じ要素の
 * まま)。
 *
 * 添付になるファイルは、画面を通らずにRust側がOSから受け取り、画面には名前だけが知らされる
 * (窓に落とした・選択画面で選んだ・クリップボードの画像)。受け取った時点で入力欄が出ていない・
 * 添付を足せないときは受け付けない(読ませないまま、次に受け取ったときに捨てられる)。
 */
export function ComposeProvider({ children }: { children: ReactNode }) {
  const [draft, setDraft] = useState('')
  const staged = useStagedAttachments()
  const receiveTarget = useRef<((files: ReceivedFiles) => void) | null>(null)
  const setReceiveTarget = useCallback((target: ((files: ReceivedFiles) => void) | null) => {
    receiveTarget.current = target
  }, [])
  const receive = useCallback((files: ReceivedFiles) => receiveTarget.current?.(files), [])

  useEffect(() => {
    // 読み込み直した画面は入力欄の添付を持たないので、Rust側に残った預かりを捨てる。
    discardAllStagedAttachments().catch(() => undefined)
    // 送り先は1つで、渡し直すと置き換わる(StrictModeで2回渡しても、後のものだけが残る)。
    watchDroppedFiles(receive).catch(() => undefined)
  }, [receive])

  return (
    <ComposeContext value={{ draft, setDraft, staged, setReceiveTarget, receive }}>
      {children}
    </ComposeContext>
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
  const { draft, setDraft, staged, setReceiveTarget, receive } = compose
  // 添付を足せるか。選ぶ・落とす・貼り付けるのどれにも同じ条件を使う。
  const canAdd = !disabled
  // 選んでいるモデルが添付を種別ごとにどう受け取るか。警告の判断はRust側が済ませてある。
  const [deliveries, setDeliveries] = useState<AttachmentDeliveries | null>(null)
  // OSの選択画面を開いている間。
  const [picking, setPicking] = useState(false)

  // 受け取ったファイルの受け取り先を、描くたびに今の`staged`へ向け直す(`addReceived`は描くたびに
  // 変わる)。応答待ちになった描画のすぐ後から受け付けないよう、画面に出す前に差し替える。選択画面・
  // 貼り付けの結果も、届いたときの入力欄の状態で受ける。
  useLayoutEffect(() => {
    setReceiveTarget(canAdd ? staged.addReceived : null)
    return () => setReceiveTarget(null)
  })

  // 本文が空でも、添付があれば送れる。判定を待っている添付があるうちは送らない。空白だけの
  // 本文で送信を押せなくするのは入力の補助で、受け付けるかはRust側が決める。
  const canSend = !disabled && !staged.busy && (draft.trim() !== '' || staged.ready)

  // 本文は打ったまま送る(前後の空白を削るかはRust側が決める)。
  const send = () => {
    if (!canSend) return
    const attachments = staged.take()
    setDraft('')
    onSend({ text: draft, attachments, restore: () => staged.restore(attachments) })
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
          <button
            type="button"
            disabled={!canAdd || picking}
            title={t('attachment.add_tooltip')}
            // 選ぶのも読むのもRust側(OSの選択画面)。画面はファイルに触れない。開いている間は
            // 押せなくする(2つ開くと、後に閉じた方の受け取りが先の分を置き換える)。
            onClick={() => {
              setPicking(true)
              void pickAttachments()
                .then(
                  (files) => {
                    if (files) receive(files)
                  },
                  (e: unknown) => onError(failureText(e)),
                )
                .finally(() => setPicking(false))
            }}
          >
            {t('attachment.add_button')}
          </button>
          <textarea
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onPaste={(e) => {
              // 文字も載っていれば文字として貼る。表計算ソフト等は、コピーしたセルの文字と
              // 一緒に、その見た目の画像も載せるため。文字(空白だけを除く)が無ければ、
              // クリップボードの画像をRust側に読ませる(画面は`clipboardData`のファイルを読まない)。
              if (e.clipboardData.getData('text/plain').trim() !== '' || !canAdd) return
              void pasteClipboardImage().then(
                (files) => {
                  if (files) receive(files)
                },
                (err: unknown) => onError(failureText(err)),
              )
            }}
            onKeyDown={(e) => {
              if (isSendEnter(e)) {
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
