import type { Message, TurnEvent } from './types'

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
///
/// 最終行が`kind='normal'`であることは`list_for_task`が保証する。通常発言が1行も残らない
/// ターン(編集で破棄されたターン)はクエリの時点で会話から外れるため、ここへ届かない
/// (Issue #95)。破棄されたかどうかの判定を表示側にも持たせると同じ判断が2箇所に分かれる
/// ので、ここでは判定しない(../../docs/spec/principles.md 5節)。
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

// `id`は描画のキー。保存済みの項目では行のid、応答待ちの間の思考では項目の位置。
export type ThoughtItem =
  | { kind: 'reasoning'; id: number; text: string }
  | { kind: 'tool'; id: number; content: ToolExecutionContent; isError: boolean }

/// ツール実行記録1件の項目。保存済みの行からも、応答待ちの間に届いた知らせからもこれで作る。
function toolItem(id: number, content: ToolExecutionContent): ThoughtItem {
  return { kind: 'tool', id, content, isError: isErrorResult(content.result) }
}

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
      items.push(toolItem(entry.id, parseToolExecution(entry.content)))
    }
  }
  return items
}

/// 応答待ちの間に届いたイベントから組み立てる思考・ツールの項目(Issue #70)。
export interface LiveThoughts {
  items: ThoughtItem[]
  /// 最後の項目が、続きの届きうる思考か。ラウンドの区切り(`done`)とツールの実行で閉じる。
  reasoningOpen: boolean
}

export const NO_LIVE_THOUGHTS: LiveThoughts = { items: [], reasoningOpen: false }

/// 届いたイベントを1件積む。保存済みのターンと同じく、1ラウンドの思考は1項目にまとめる。
/// 本文は描かない(完了後に読み直した返信で出す。ライブ表示は#204)。実行前のツール呼び出しも
/// 描かず、実行の知らせ(`tool_executed`)で結果と一緒に出す。
export function appendTurnEvent(live: LiveThoughts, event: TurnEvent): LiveThoughts {
  if (event.type === 'tool_executed') {
    return { items: [...live.items, toolItem(event.id, event.record)], reasoningOpen: false }
  }
  const response = event.event
  switch (response.type) {
    case 'reasoning_delta': {
      const last = live.items[live.items.length - 1]
      if (live.reasoningOpen && last?.kind === 'reasoning') {
        const merged = { ...last, text: last.text + response.text }
        return { items: [...live.items.slice(0, -1), merged], reasoningOpen: true }
      }
      const opened: ThoughtItem = { kind: 'reasoning', id: live.items.length, text: response.text }
      return { items: [...live.items, opened], reasoningOpen: true }
    }
    case 'done':
      return { ...live, reasoningOpen: false }
    case 'text_delta':
    case 'tool_call':
      return live
  }
}
