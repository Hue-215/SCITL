import { memo, useRef, useState } from 'react'
import ReactMarkdown, { type Components } from 'react-markdown'
import remarkGfm from 'remark-gfm'
import LinkDialog from './LinkDialog'
import remarkInertHtml from './remarkInertHtml'
import remarkSoftBreaks from './remarkSoftBreaks'

// remarkSoftBreaksは、remarkInertHtmlが`<br>`から作った改行を見て二重の改行を避けるため後に置く
const REMARK_PLUGINS = [remarkGfm, remarkInertHtml, remarkSoftBreaks]

// 発言本文のMarkdown描画。react-markdownはHTML文字列を経由せずReactの
// 要素を直接組み立てるため、innerHTMLへの注入経路を持たない。生のHTML・画像は
// remarkInertHtmlが構文木の段階で無害化する。
//
// 本文が変わらない限り描き直さない(`memo`)。会話欄は入力欄と同じ親の下にあり、1文字打つ
// たびに全発言を解析し直すと、会話が長いほど入力が重くなるため。
export default memo(function Markdown({ text }: { text: string }) {
  const [linkUrl, setLinkUrl] = useState<string | null>(null)
  const rootRef = useRef<HTMLDivElement>(null)

  const components: Components = {
    // リンクはWebView内で遷移させず、必ず確認ダイアログを経てOSのブラウザで開く。
    // <a>にhrefを持たせないことで、クリック処理以外の経路
    // (中クリック・ドラッグ・右クリックメニュー・エンジンによるDNS先読み)をまとめて無くす。
    // 確認には書かれたURLをそのまま渡し、許可されない通信方式でも理由を表示できるようにする。
    a: ({ href, children }) => {
      const activate = () => {
        if (href === undefined) return
        // 脚注などページ内への参照は、同じ発言の中の該当箇所へ移るだけにする
        // (idは発言ごとに重複しうるため、文書全体ではなくこの発言の中から探す)
        if (href.startsWith('#')) {
          const id = decodeURIComponent(href.slice(1))
          rootRef.current?.querySelector(`[id="${CSS.escape(id)}"]`)?.scrollIntoView({ block: 'nearest' })
          return
        }
        setLinkUrl(href)
      }
      return (
        <a
          role="link"
          tabIndex={0}
          title={href}
          onClick={activate}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault()
              activate()
            }
          }}
        >
          {children}
        </a>
      )
    },
  }

  return (
    <div className="markdown" ref={rootRef}>
      <ReactMarkdown
        remarkPlugins={REMARK_PLUGINS}
        // 無害化の対象から漏れたHTML・画像が万一残っても描画しない
        skipHtml
        disallowedElements={['img']}
        // URLはhref属性に置かず、確認ダイアログにだけ渡す(上のa)。既定の無害化は
        // javascript:等を空にしてしまい、開けない理由を表示できなくなるため素通しにする
        urlTransform={(url) => url}
        components={components}
      >
        {text}
      </ReactMarkdown>
      {linkUrl !== null && <LinkDialog url={linkUrl} onClose={() => setLinkUrl(null)} />}
    </div>
  )
})
