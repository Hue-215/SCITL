import { t } from './i18n'
import { operationSourceLabel, type ThoughtItem } from './thinking'
import type { Message, ToolExecutionView } from './types'

// 「思考・ツール」の折りたたみ表示(Issue #42、docs/spec/legacy/frontend.md 1節)。
// モデルの思考(reasoning)と内部ツール呼び出しを発生順に混在させて表示する。
// 応答生成以外の経路(画面・MCP等)での操作の記録はここに含めず、独立した1行として扱う
// (`OperationLine`)。
// 保存済みのターンも、応答待ちの間の途中経過(Issue #70)もこれで描く。
//
// 表示専用のコンポーネントであり、モデルへの再送信経路には一切関与しない
// (docs/spec/principles.md 3節「思考は履歴に送り返さない」。思考はAPIへ送る
// ChatMessageの構成要素として存在しないため、バックエンド側で型として遮断されている)。
//
// 思考・ツール引数・結果はすべてプレーンテキストとして描画する(JSXのテキスト補間と
// <pre>のみを使い、dangerouslySetInnerHTMLは使わない)。いずれもモデルや外部ツールが
// 出したものをそのまま確かめるための表示なので、本文用のMarkdown描画(Markdown.tsx)は
// 通さない。整形しない分、ここからリンクや画像が作られることも無い。引数・結果は
// Rust側が整形し、見えない文字を見える形にしてある(`ToolExecutionView`)。

// ツール呼び出し1件の引数と結果。ターンの中の呼び出しと操作の記録のどちらでも同じ形で見せる。
function ToolCallDetail({ execution }: { execution: ToolExecutionView }) {
  return (
    <div className="tool-call-detail">
      <p className="tool-call-label">{t('chat.tool_detail_args')}</p>
      <pre>{execution.arguments}</pre>
      <p className="tool-call-label">{t('chat.tool_detail_result')}</p>
      <pre>{execution.result}</pre>
    </div>
  )
}

// ツール名はモデルが書いたものなので、続く「()」や失敗の印の並びを入れ替えないよう閉じ込める
// (ui.md 2節「部品ごとの決まり」)。
function ToolName({ execution }: { execution: ToolExecutionView }) {
  return <bdi>{execution.tool ?? t('chat.tool_unknown')}</bdi>
}

export function ThinkingTools({ items }: { items: ThoughtItem[] }) {
  if (items.length === 0) return null

  const hasError = items.some((item) => item.kind === 'tool' && item.execution.is_error)

  return (
    <details className="thinking-tools">
      <summary>
        {t('chat.thinking_tools_count', { count: items.length })}
        {hasError && (
          <span className="thinking-tools-error">{t('chat.thinking_tools_has_error')}</span>
        )}
      </summary>
      <ol className="thinking-tools-list">
        {items.map((item) =>
          item.kind === 'reasoning' ? (
            <li key={`reasoning-${item.id}`} className="thinking-item">
              {item.text}
            </li>
          ) : (
            <li key={`tool-${item.id}`}>
              <details className="tool-call">
                <summary>
                  <ToolName execution={item.execution} />()
                  {item.execution.is_error && (
                    <span className="thinking-tools-error">{t('chat.tool_error')}</span>
                  )}
                </summary>
                <ToolCallDetail execution={item.execution} />
              </details>
            </li>
          ),
        )}
      </ol>
    </details>
  )
}

/// 応答生成以外の経路(画面・MCP等)での操作の記録1件を、「思考・ツール」折りたたみとは
/// 独立した1行として表示する。行末に経路のラベルを出し、ターンの中の呼び出しと見分けられる
/// ようにする(docs/spec/legacy/frontend.md 1節)。
export function OperationLine({ message }: { message: Message }) {
  const execution = message.tool_execution
  if (!execution) return null
  return (
    <details className="operation-line">
      <summary>
        <span className="operation-name">
          <ToolName execution={execution} />()
        </span>
        {execution.is_error && <span className="thinking-tools-error">{t('chat.tool_error')}</span>}
        <span className="operation-label">{t(operationSourceLabel(message.source))}</span>
      </summary>
      <ToolCallDetail execution={execution} />
    </details>
  )
}
