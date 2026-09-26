import { t } from './i18n'
import {
  isErrorResult,
  parseToolExecution,
  type ThoughtItem,
  type ToolExecutionContent,
} from './thinking'
import type { Message } from './types'

// 「思考・ツール」の折りたたみ表示(Issue #42、docs/spec/legacy/frontend.md 1節)。
// モデルの思考(reasoning)と内部ツール呼び出しを発生順に混在させて表示する。
// 外部(MCP)経由のツール呼び出しはここに含めず、App.tsx側で独立した1行として扱う。
// 保存済みのターンも、応答待ちの間の途中経過(Issue #70)もこれで描く。
//
// 表示専用のコンポーネントであり、モデルへの再送信経路には一切関与しない
// (docs/spec/principles.md 3節「思考は履歴に送り返さない」。思考はAPIへ送る
// ChatMessageの構成要素として存在しないため、バックエンド側で型として遮断されている)。
//
// 思考・ツール引数・結果はすべてプレーンテキストとして描画する(JSXのテキスト補間と
// <pre>のみを使い、dangerouslySetInnerHTMLは使わない)。いずれもモデルや外部ツールが
// 出したものをそのまま確かめるための表示なので、本文用のMarkdown描画(Markdown.tsx)は
// 通さない。整形しない分、ここからリンクや画像が作られることも無い。

// ツール呼び出し1件の引数と結果。内部・外部(MCP)のどちらの表示でも同じ形で見せる。
function ToolCallDetail({ content }: { content: ToolExecutionContent }) {
  return (
    <div className="tool-call-detail">
      <p className="tool-call-label">{t('chat.tool_detail_args')}</p>
      <pre>{JSON.stringify(content.arguments ?? {}, null, 2)}</pre>
      <p className="tool-call-label">{t('chat.tool_detail_result')}</p>
      <pre>{JSON.stringify(content.result ?? null, null, 2)}</pre>
    </div>
  )
}

export function ThinkingTools({ items }: { items: ThoughtItem[] }) {
  if (items.length === 0) return null

  const hasError = items.some((item) => item.kind === 'tool' && item.isError)

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
                  {item.content.tool ?? t('chat.tool_unknown')}()
                  {item.isError && <span className="thinking-tools-error">{t('chat.tool_error')}</span>}
                </summary>
                <ToolCallDetail content={item.content} />
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
        <span className="external-tool-name">{content.tool ?? t('chat.tool_unknown')}()</span>
        {isError && <span className="thinking-tools-error">{t('chat.tool_error')}</span>}
        <span className="external-tool-label">{t('chat.tool_external_label')}</span>
      </summary>
      <ToolCallDetail content={content} />
    </details>
  )
}
