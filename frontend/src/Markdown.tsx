import { isValidElement, memo, useRef, type ReactNode } from 'react'
import ReactMarkdown, { type Components } from 'react-markdown'
import remarkGfm from 'remark-gfm'
import { openLink } from './api'
import CodeBlock from './CodeBlock'
import remarkInertHtml from './remarkInertHtml'
import remarkSoftBreaks from './remarkSoftBreaks'

// remarkSoftBreaksは、remarkInertHtmlが`<br>`から作った改行を見て二重の改行を避けるため後に置く
const REMARK_PLUGINS = [remarkGfm, remarkInertHtml, remarkSoftBreaks]

// コードブロックの言語名(```の後ろに書かれた最初の語)。<pre>の中の<code>に`language-…`の
// クラスとして載ってくる。モデルが書くものなので、言語名らしい短い英数字のときだけ出す
// (見出しの行に文を書いて、SCITL自身の表示を装えないように)。
const LANGUAGE_CLASS = /(?:^|\s)language-([A-Za-z0-9+#._-]{1,32})(?:\s|$)/

function languageOf(children: ReactNode): string | null {
  if (!isValidElement<{ className?: string }>(children)) return null
  return LANGUAGE_CLASS.exec(children.props.className ?? '')?.[1] ?? null
}

// コードブロックの描き方。描くたびに作り直すと開閉の状態が消えるので、モジュールに1つずつ置く。
function Pre({ children }: { children?: ReactNode }) {
  return <CodeBlock label={languageOf(children)}>{children}</CodeBlock>
}

function StreamingPre({ children }: { children?: ReactNode }) {
  return (
    <CodeBlock label={languageOf(children)} collapse={false}>
      {children}
    </CodeBlock>
  )
}

// 発言本文のMarkdown描画。react-markdownはHTML文字列を経由せずReactの要素を直接組み
// 立てるため、innerHTMLへの注入経路を持たない。生のHTML・画像はremarkInertHtmlが構文木の
// 段階で無害化する。
//
// 本文が変わらない限り描き直さない(`memo`)。会話欄は入力欄と同じ親の下にあり、1文字打つ
// たびに全発言を解析し直すと、会話が長いほど入力が重くなるため。
//
// `streaming`は、書いている途中の本文か。途中の間は長いコードブロックを畳まない。
export default memo(function Markdown({
  text,
  streaming = false,
}: {
  text: string
  streaming?: boolean
}) {
  const rootRef = useRef<HTMLDivElement>(null)

  const components: Components = {
    pre: streaming ? StreamingPre : Pre,
    // リンクはWebView内で遷移させず、Rust側が出す確認のダイアログを経てOSのブラウザで開く。<a>に
    // hrefを持たせないことで、クリック処理以外の経路(中クリック・ドラッグ・右
    // クリックメニュー・エンジンによるDNS先読み)をまとめて無くす。書かれたURLをそのまま
    // 渡し、許可されない通信方式でも理由をRust側が知らせられるようにする。
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
        // 開けないリンクとOSへ渡す失敗はRust側がダイアログで知らせる。コマンド自体の失敗(起動に
        // 失敗した画面等)は、本文の中に出す場所が無いので出さない。
        void openLink(href).catch(() => undefined)
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
        // URLはhref属性に置かず、開く操作にだけ渡す(上のa)。既定の無害化は
        // javascript:等を空にしてしまい、開けない理由を表示できなくなるため素通しにする
        urlTransform={(url) => url}
        components={components}
      >
        {text}
      </ReactMarkdown>
    </div>
  )
})
