import type { MessageKey } from './i18n'
import type { MessageView, ToolExecutionView, TurnEvent } from './types'

// 「思考・ツール」の折りたたみ表示のためのデータ整形ロジック(Issue #42)。
// コンポーネント本体は./ThinkingTools.tsxに置き、こちらは純粋な変換関数のみを持つ
// (react/only-export-componentsに合わせてコンポーネントと非コンポーネントのエクスポートを
// ファイルごとに分ける)。

export interface TurnGroup {
  kind: 'turn'
  turnId: string
  entries: MessageView[]
}

export interface PlainEntry {
  kind: 'plain'
  message: MessageView
}

export type DisplayItem = TurnGroup | PlainEntry

/**
 * `list_chat_messages`が返す発言列(created_at, id順)を、SCITL自身の応答生成に属する行
 * (`turn_id`が同じ行)ごとにまとめる。ユーザー発言・応答生成以外の経路での操作の
 * 記録は`turn_id`を持たないため常に独立した`plain`項目になる
 * (docs/spec/rebuild/data-model.md「ターン境界」の3分類)。
 *
 * 連続した行ではなく`turn_id`でまとめ、ターンは最初の行の位置に1つだけ置く。別プロセスの
 * 操作の記録がターンの途中に挟まりうるため(data-model.md「応答生成以外の経路での操作の記録」)。
 */
export function groupMessages(messages: MessageView[]): DisplayItem[] {
  const items: DisplayItem[] = []
  const turns = new Map<string, TurnGroup>()
  for (const message of messages) {
    if (message.turn_id === null) {
      items.push({ kind: 'plain', message })
      continue
    }
    const turn = turns.get(message.turn_id)
    if (turn) {
      turn.entries.push(message)
      continue
    }
    const opened: TurnGroup = { kind: 'turn', turnId: message.turn_id, entries: [message] }
    turns.set(message.turn_id, opened)
    items.push(opened)
  }
  return items
}

/**
 * 1ターン分のentriesのうち、実際に見える返信の吹き出しになる行(最終行)。
 * 内部ツール実行を除く最後の行(通常応答 or エラー発言)。
 *
 * 最終行が`kind='normal'`であることは`list_for_chat`が保証する。通常発言が1行も残らない
 * ターン(編集で破棄されたターン)はクエリの時点で会話から外れるため、ここへ届かない
 * (Issue #95)。破棄されたかどうかの判定を表示側にも持たせると同じ判断が2箇所に分かれる
 * ので、ここでは判定しない(../../docs/spec/principles.md 5節)。
 */
export function finalEntryOf(entries: MessageView[]): MessageView {
  return entries[entries.length - 1]
}

/**
 * 操作の記録の行末に出す経路のラベル(`messages.source`)。知らない値は汎用のラベルにし、
 * 値そのものは出さない(MCP経由の`mcp:`の後ろは外部のクライアントが名乗る名前になる)。
 */
export function operationSourceLabel(source: string | null): MessageKey {
  if (source === 'ui') return 'chat.source_ui'
  if (source === 'cli') return 'chat.source_cli'
  if (source === 'mcp' || source?.startsWith('mcp:')) return 'chat.source_mcp'
  return 'chat.source_unknown'
}

/** `id`は描画のキー。保存済みの項目では行のid、応答待ちの間の思考では項目の位置。 */
export type ThoughtItem =
  | { kind: 'reasoning'; id: number; text: string }
  | { kind: 'tool'; id: number; execution: ToolExecutionView }

/**
 * 1ターン分のentriesから、発生順の思考・ツール項目列を組み立てる。各行の`reasoning`は
 * そのラウンド(または最終応答)より前に生じた思考であるため、同じ行のツール実行より
 * 先に並べる(`orchestration/turn.rs`がラウンド内最初のツール実行記録の`reasoning`列に
 * そのラウンドの思考を格納する設計と対応する)。
 */
export function buildThoughtItems(entries: MessageView[]): ThoughtItem[] {
  const items: ThoughtItem[] = []
  for (const entry of entries) {
    if (entry.reasoning) {
      items.push({ kind: 'reasoning', id: entry.id, text: entry.reasoning })
    }
    if (entry.tool_execution) {
      items.push({ kind: 'tool', id: entry.id, execution: entry.tool_execution })
    }
  }
  return items
}

/** 応答待ちの間に届いたイベントから組み立てる思考・ツールの項目(Issue #70)。 */
export interface LiveThoughts {
  items: ThoughtItem[]
  /** 最後の項目が、続きの届きうる思考か。ラウンドの区切り(`done`)とツールの実行で閉じる。 */
  reasoningOpen: boolean
}

export const NO_LIVE_THOUGHTS: LiveThoughts = { items: [], reasoningOpen: false }

/**
 * 届いたイベントを1件積む。保存済みのターンと同じく、1ラウンドの思考は1項目にまとめる。
 * 本文は描かない(完了後に読み直した返信で出す。ライブ表示は#204)。実行前のツール呼び出しも
 * 描かず、実行の知らせ(`tool_executed`)で結果と一緒に出す。
 */
export function appendTurnEvent(live: LiveThoughts, event: TurnEvent): LiveThoughts {
  if (event.type === 'tool_executed') {
    const item: ThoughtItem = { kind: 'tool', id: event.id, execution: event.execution }
    return { items: [...live.items, item], reasoningOpen: false }
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
