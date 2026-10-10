import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react'
import { t, type MessageKey } from './i18n'
import { CollapseToggle } from './settingsFields'

// コピーの結果をボタンの文言で知らせておく時間。
const COPY_NOTICE_MS = 2000

const COPY_LABELS = {
  idle: 'common.copy',
  done: 'common.copied',
  failed: 'common.copy_failed',
} as const satisfies Record<string, MessageKey>

/**
 * コードの塊。見出しの行(`label`とコピーのボタン)の下に中身を等幅で出し、長いものは先頭だけを
 * 見せて畳む。発言本文のコードブロックと、エラー発言の詳細で使う。
 *
 * 畳むかは行数ではなく描いた高さで決める(折り返して出す中身は、改行が無くても長くなるため)。
 * 高さの上限はCSSが持ち(`--code-collapsed-max-height`)、ここでは収まっているかだけを測る。
 * 測るのは描いたあとなので、開閉のボタンを足しても塊の高さが変わらないよう、ボタンの分だけ中身を
 * 低くする(高さが変わると、下端に合わせたあとの会話欄がずれる)。
 * `collapse`を偽にすると畳まない(書いている途中の本文。伸びていく先が隠れるため)。
 */
export default function CodeBlock({
  label = null,
  collapse = true,
  className,
  children,
}: {
  // モデルが書いた文字列でもよい(ボタンと同じ行に並ぶので、ここで閉じ込める)。
  label?: string | null
  collapse?: boolean
  className?: string
  children: ReactNode
}) {
  const bodyRef = useRef<HTMLPreElement>(null)
  const [expanded, setExpanded] = useState(false)
  const [overflowing, setOverflowing] = useState(false)
  const [copy, setCopy] = useState<keyof typeof COPY_LABELS>('idle')
  const clipped = collapse && !expanded

  // 収まっているかを、描くたび(中身が変わるたび)と、幅が変わって折り返しが変わるたびに測り直す。
  // 開いている間は上限が掛かっておらず測れないので、前の結果を保つ(畳み直したときに測る)。
  // 高さは整数に丸めて届くので、1pxの差は収まっているとみなす。
  const measure = () => {
    const body = bodyRef.current
    if (body && clipped) setOverflowing(body.scrollHeight > body.clientHeight + 1)
  }
  const measureRef = useRef(measure)
  useLayoutEffect(() => {
    measureRef.current = measure
    measure()
  })
  useEffect(() => {
    const body = bodyRef.current
    if (!body || !clipped) return
    const observer = new ResizeObserver(() => measureRef.current())
    observer.observe(body)
    return () => observer.disconnect()
  }, [clipped])

  useEffect(() => {
    if (copy === 'idle') return
    const timer = setTimeout(() => setCopy('idle'), COPY_NOTICE_MS)
    return () => clearTimeout(timer)
  }, [copy])

  // 畳んで見えていない続きも含めて、中身の全部をクリップボードへ書く。書くだけで、読まない
  // (webview-boundary.md「画面が持つもの・持たないもの」)。
  const copyText = async () => {
    // Markdownのコードブロックは末尾に改行が付く。端末に貼ったときにそのまま実行されないよう除く。
    const text = (bodyRef.current?.textContent ?? '').replace(/\n$/, '')
    try {
      // Clipboard APIが無い環境の例外も、書けなかった扱いにまとめる。
      await navigator.clipboard.writeText(text)
      setCopy('done')
    } catch {
      setCopy('failed')
    }
  }

  const bodyClass = !clipped
    ? undefined
    : overflowing
      ? 'code-block-clipped code-block-overflowing'
      : 'code-block-clipped'
  return (
    <div className={className ? `code-block ${className}` : 'code-block'}>
      <div className="code-block-bar">
        {label && (
          <span className="code-block-label">
            <bdi>{label}</bdi>
          </span>
        )}
        <button
          type="button"
          className="code-block-copy"
          aria-live="polite"
          onClick={() => void copyText()}
        >
          {t(COPY_LABELS[copy])}
        </button>
      </div>
      <pre ref={bodyRef} className={bodyClass}>
        {children}
      </pre>
      {collapse && (overflowing || expanded) && (
        <CollapseToggle
          showLabel={t('common.show_all')}
          expanded={expanded}
          onToggle={() => setExpanded((v) => !v)}
        />
      )}
    </div>
  )
}
