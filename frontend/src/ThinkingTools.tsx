import type { ReactNode } from 'react'
import { isolated, t } from './i18n'
import { DisclosureMark } from './Icon'
import { operationSourceLabel, type ThoughtItem } from './thinking'
import type { MessageView, ToolExecutionView } from './types'

// ターンの中の思考とツール呼び出しの表示。続いた思考は「思考」の小さな折りたたみ1つにまとめ、
// ツール呼び出しは1件ずつ、名前と引数の要約の1行で出す(押すと引数・結果が開く)。何をしたかを
// 開かずに追えるようにするため。応答生成以外の経路(画面・MCP等)での操作の記録も同じ1行で、
// 行末に経路のラベルを付けて見分ける(`OperationLine`)。保存済みのターンも、応答待ちの間の
// 途中経過もこれで描く。
//
// 思考・ツール引数・結果はすべてプレーンテキストとして描画する(dangerouslySetInnerHTMLも
// Markdown描画も使わない)。モデルや外部ツールが出したものをそのまま確かめるための表示で、
// ここからリンクや画像を作らせないため。引数・結果・要約・失敗の文言はRust側が整形し、見えない
// 文字を見える形にしてある(`ToolExecutionView`)。

/** ツール呼び出し1件の引数と結果。ターンの中の呼び出しと操作の記録のどちらでも同じ形で見せる。 */
function ToolCallDetail({ execution }: { execution: ToolExecutionView }) {
  return (
    <div className="tool-call-detail detail-box">
      <p className="tool-call-label">{t('chat.tool_detail_args')}</p>
      <pre>{execution.arguments}</pre>
      <p className="tool-call-label">{t('chat.tool_detail_result')}</p>
      <pre>{execution.result}</pre>
    </div>
  )
}

/**
 * ツール呼び出し1件の1行。名前と引数の要約を出し、失敗なら失敗の文言を添える。`label`は行末に
 * 足すもの(操作の記録の経路)。モデルが書いた名前・要約・文言は、並びを入れ替えないよう
 * それぞれ閉じ込める。
 */
function ToolLine({
  execution,
  label = null,
}: {
  execution: ToolExecutionView
  label?: ReactNode
}) {
  return (
    <details className="tool-call">
      <summary>
        <DisclosureMark />
        <span className="tool-call-name">
          <bdi>{execution.tool ?? t('chat.tool_unknown')}</bdi>
        </span>
        {execution.summary && (
          <span className="tool-call-summary">
            <bdi>{execution.summary}</bdi>
          </span>
        )}
        {execution.is_error && (
          <span className="tool-call-error">
            {execution.error === null
              ? t('chat.tool_failed_plain')
              : t('chat.tool_failed', { error: isolated(execution.error) })}
          </span>
        )}
        {label}
      </summary>
      <ToolCallDetail execution={execution} />
    </details>
  )
}

/** 続いた思考の折りたたみ。 */
function Thinking({ texts }: { texts: { id: number; text: string }[] }) {
  return (
    <details className="thinking">
      <summary>
        <DisclosureMark />
        {t('chat.thinking')}
      </summary>
      <div className="thinking-list detail-box">
        {texts.map((item) => (
          <p key={item.id} className="thinking-item">
            {item.text}
          </p>
        ))}
      </div>
    </details>
  )
}

/** 思考・ツールの項目を、起きた順に思考の折りたたみとツールの1行に分けて並べる。 */
export function ThinkingTools({ items }: { items: ThoughtItem[] }) {
  const blocks: ReactNode[] = []
  let thinking: { id: number; text: string }[] = []
  const closeThinking = () => {
    if (thinking.length === 0) return
    blocks.push(<Thinking key={`reasoning-${thinking[0].id}`} texts={thinking} />)
    thinking = []
  }
  for (const item of items) {
    if (item.kind === 'reasoning') {
      thinking.push(item)
      continue
    }
    closeThinking()
    blocks.push(<ToolLine key={`tool-${item.id}`} execution={item.execution} />)
  }
  closeThinking()
  return <>{blocks}</>
}

/**
 * 応答生成以外の経路(画面・MCP等)での操作の記録1件。ターンの中の呼び出しと同じ1行に、
 * 経路のラベルを添えて見分けられるようにする。
 */
export function OperationLine({ message }: { message: MessageView }) {
  const execution = message.tool_execution
  if (!execution) return null
  return (
    <ToolLine
      execution={execution}
      label={<span className="operation-label">{t(operationSourceLabel(message.source))}</span>}
    />
  )
}
