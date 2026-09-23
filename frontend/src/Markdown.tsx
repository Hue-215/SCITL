import { useState } from 'react'
import ReactMarkdown, { defaultUrlTransform, type Components } from 'react-markdown'
import remarkGfm from 'remark-gfm'
import LinkDialog from './LinkDialog'
import remarkInertHtml from './remarkInertHtml'

const REMARK_PLUGINS = [remarkGfm, remarkInertHtml]

// 発言本文のMarkdown描画(Issue #39)。react-markdownはHTML文字列を経由せずReactの
// 要素を直接組み立てるため、innerHTMLへの注入経路を持たない。生のHTML・画像は
// remarkInertHtmlが構文木の段階で無害化する。
export default function Markdown({ text }: { text: string }) {
  const [linkUrl, setLinkUrl] = useState<string | null>(null)

  const components: Components = {
    // リンクはWebView内で遷移させず、必ず確認ダイアログを経てOSのブラウザで開く
    // (principles.md 4節)。確認には書かれたURLをそのまま渡し、許可されない通信方式でも
    // 理由を表示できるようにする。一方href属性には既定の無害化(javascript:等を空にする)を
    // 通した値だけを置き、クリック処理が漏れた場合にも危険なURLが実行されないようにする。
    a: ({ href, children }) => {
      const open = () => setLinkUrl(href ?? '')
      return (
        <a
          href={defaultUrlTransform(href ?? '') || undefined}
          tabIndex={0}
          onClick={(e) => {
            e.preventDefault()
            open()
          }}
          onAuxClick={(e) => e.preventDefault()}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault()
              open()
            }
          }}
        >
          {children}
        </a>
      )
    },
  }

  return (
    <div className="markdown">
      <ReactMarkdown
        remarkPlugins={REMARK_PLUGINS}
        // 無害化の対象から漏れたHTML・画像が万一残っても描画しない
        skipHtml
        disallowedElements={['img']}
        // hrefの無害化は上のaで行う(確認ダイアログに元のURLを渡すため、ここでは素通しにする)
        urlTransform={(url) => url}
        components={components}
      >
        {text}
      </ReactMarkdown>
      {linkUrl !== null && <LinkDialog url={linkUrl} onClose={() => setLinkUrl(null)} />}
    </div>
  )
}
