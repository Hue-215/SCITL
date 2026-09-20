import { buildThoughtItems, isErrorResult, parseToolExecution } from './thinking'
import type { Message } from './types'

// 「思考・ツール」の折りたたみ表示(Issue #42、docs/spec/legacy/frontend.md 1節)。
// モデルの思考(reasoning)と内部ツール呼び出しを発生順に混在させて表示する。
// 外部(MCP)経由のツール呼び出しはここに含めず、App.tsx側で独立した1行として扱う。
//
// 表示専用のコンポーネントであり、モデルへの再送信経路には一切関与しない
// (docs/spec/principles.md 3節「思考は履歴に送り返さない」。思考はAPIへ送る
// ChatMessageの構成要素として存在しないため、バックエンド側で型として遮断されている)。
//
// 思考・ツール引数・結果はすべてプレーンテキストとして描画する(JSXのテキスト補間と
// <pre>のみを使い、dangerouslySetInnerHTMLは使わない)。Markdown描画は別Issue #39の
// 範囲であり、ここでは扱わない。

export function ThinkingTools({ entries }: { entries: Message[] }) {
  const items = buildThoughtItems(entries)
  if (items.length === 0) return null

  const hasError = items.some((item) => item.kind === 'tool' && item.isError)

  return (
    <details className="thinking-tools">
      <summary>
        思考・ツール({items.length}件){hasError && <span className="thinking-tools-error">・エラーあり</span>}
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
                  {item.content.tool ?? '不明なツール'}()
                  {item.isError && <span className="thinking-tools-error">・エラー</span>}
                </summary>
                <div className="tool-call-detail">
                  <p className="tool-call-label">引数</p>
                  <pre>{JSON.stringify(item.content.arguments ?? {}, null, 2)}</pre>
                  <p className="tool-call-label">結果</p>
                  <pre>{JSON.stringify(item.content.result ?? null, null, 2)}</pre>
                </div>
              </details>
            </li>
          ),
        )}
      </ol>
    </details>
  )
}

/// 外部(MCP)経由のツール呼び出し1件を、内部の「思考・ツール」折りたたみとは独立した
/// 1行として表示する(docs/spec/legacy/frontend.md 1節)。
export function ExternalToolLine({ message }: { message: Message }) {
  const content = parseToolExecution(message.content)
  const isError = isErrorResult(content.result)
  return (
    <details className="external-tool-line">
      <summary>
        <span className="external-tool-name">{content.tool ?? '不明なツール'}()</span>
        {isError && <span className="thinking-tools-error">・エラー</span>}
        <span className="external-tool-label">MCP</span>
      </summary>
      <div className="tool-call-detail">
        <p className="tool-call-label">引数</p>
        <pre>{JSON.stringify(content.arguments ?? {}, null, 2)}</pre>
        <p className="tool-call-label">結果</p>
        <pre>{JSON.stringify(content.result ?? null, null, 2)}</pre>
      </div>
    </details>
  )
}
