import type { Message } from './types'

// 「思考・ツール」の折りたたみ表示のためのデータ整形ロジック(Issue #42)。
// コンポーネント本体は./ThinkingTools.tsxに置き、こちらは純粋な変換関数のみを持つ
// (react/only-export-componentsに合わせてコンポーネントと非コンポーネントのエクスポートを
// ファイルごとに分ける)。

export interface TurnGroup {
  kind: 'turn'
  turnId: string
  entries: Message[]
}

export interface PlainEntry {
  kind: 'plain'
  message: Message
}

export type DisplayItem = TurnGroup | PlainEntry

/// `list_for_task`が返す発言列(created_at, id順)を、SCITL自身の応答生成に属する行
/// (`turn_id`が同じ行の連続)ごとにまとめる。ユーザー発言・外部(MCP)経由の記録は
/// `turn_id`を持たないため常に独立した`plain`項目になる
/// (docs/spec/rebuild/data-model.md「ターン境界」の3分類)。
export function groupMessages(messages: Message[]): DisplayItem[] {
  const items: DisplayItem[] = []
  for (const message of messages) {
    if (message.turn_id === null) {
      items.push({ kind: 'plain', message })
      continue
    }
    const last = items[items.length - 1]
    if (last?.kind === 'turn' && last.turnId === message.turn_id) {
      last.entries.push(message)
    } else {
      items.push({ kind: 'turn', turnId: message.turn_id, entries: [message] })
    }
  }
  return items
}

/// 1ターン分のentriesのうち、実際に見える返信の吹き出しになる行(最終行)。
/// 内部ツール実行を除く最後の行(通常応答 or エラー発言)。
export function finalEntryOf(entries: Message[]): Message {
  return entries[entries.length - 1]
}

export interface ToolExecutionContent {
  tool?: string
  arguments?: unknown
  result?: unknown
}

export function parseToolExecution(content: string): ToolExecutionContent {
  try {
    return JSON.parse(content) as ToolExecutionContent
  } catch {
    return {}
  }
}

export function isErrorResult(result: unknown): boolean {
  return typeof result === 'object' && result !== null && 'error' in (result as Record<string, unknown>)
}

export type ThoughtItem =
  | { kind: 'reasoning'; id: number; text: string }
  | { kind: 'tool'; id: number; content: ToolExecutionContent; isError: boolean }

/// 1ターン分のentriesから、発生順の思考・ツール項目列を組み立てる。各行の`reasoning`は
/// そのラウンド(または最終応答)より前に生じた思考であるため、同じ行のツール実行より
/// 先に並べる(`orchestration/turn.rs`がラウンド内最初のツール実行記録の`reasoning`列に
/// そのラウンドの思考を格納する設計と対応する)。
export function buildThoughtItems(entries: Message[]): ThoughtItem[] {
  const items: ThoughtItem[] = []
  for (const entry of entries) {
    if (entry.reasoning) {
      items.push({ kind: 'reasoning', id: entry.id, text: entry.reasoning })
    }
    if (entry.kind === 'tool_execution') {
      const content = parseToolExecution(entry.content)
      items.push({ kind: 'tool', id: entry.id, content, isError: isErrorResult(content.result) })
    }
  }
  return items
}
