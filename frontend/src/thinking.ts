import type { MessageKey } from './i18n'
import type { MessageView, ToolExecutionView, TurnEvent } from './types'

// 「思考・ツール」の折りたたみ表示のためのデータ整形。コンポーネント本体は./ThinkingTools.tsxに
// 置く(react/only-export-componentsに合わせてファイルを分ける)。

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
 * 記録は`turn_id`を持たないため常に独立した`plain`項目になる。
 *
 * 連続した行ではなく`turn_id`でまとめ、ターンは最初の行の位置に1つだけ置く。別プロセスの
 * 操作の記録がターンの途中に挟まりうるため。
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
 * 1ターン分のentriesのうち、返信の行(最終行。通常応答 or エラー発言)。ターンの中身(`parts`)を持ち、
 * 再試行・削除の対象になる。
 *
 * 最終行が`kind='normal'`であることは`list_for_chat`が保証する(破棄されたターンは
 * ここへ届かないので、表示側では判定しない)。
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

/** `id`は描画のキー。保存済みのツールでは実行記録の行のid、ほかは項目の位置。 */
export type ThoughtItem =
  | { kind: 'reasoning'; id: number; text: string }
  | { kind: 'tool'; id: number; execution: ToolExecutionView }

/** ターンの中身を、起きた順に「思考・ツール」の折りたたみと本文の吹き出しに分けたもの。 */
export type TurnSegment =
  | { kind: 'thoughts'; items: ThoughtItem[] }
  | { kind: 'text'; id: number; text: string }

/**
 * 1ターン分のentriesから、起きた順の折りたたみと本文の列を組み立てる。返信の行(最終行)の
 * 中身(`parts`)を順に読み、続く思考・ツールは1つの折りたたみにまとめ、本文はラウンドごとの
 * 吹き出しにする。前置き→ツール→本題の順がそのまま残る。
 *
 * 最終行より前の行(返信の中身から指されていない実行記録。別の版で書かれた行等)は、先頭の
 * 折りたたみに入れる。
 */
export function buildTurnSegments(entries: MessageView[]): TurnSegment[] {
  const segments: TurnSegment[] = []
  const thought = (item: ThoughtItem) => {
    const last = segments[segments.length - 1]
    if (last?.kind === 'thoughts') last.items.push(item)
    else segments.push({ kind: 'thoughts', items: [item] })
  }
  for (const entry of entries.slice(0, -1)) {
    if (entry.tool_execution) {
      thought({ kind: 'tool', id: entry.id, execution: entry.tool_execution })
    }
  }
  finalEntryOf(entries).parts.forEach((part, index) => {
    switch (part.type) {
      case 'reasoning':
        thought({ kind: 'reasoning', id: index, text: part.text })
        return
      case 'tool':
        thought({ kind: 'tool', id: part.id, execution: part.execution })
        return
      case 'text':
        segments.push({ kind: 'text', id: index, text: part.text })
    }
  })
  return segments
}

/**
 * 応答待ちの間に届いたイベントから組み立てるターンの中身。保存済みのターンと同じ区切り
 * (`TurnSegment`)で、届いた順に並べる。
 */
export interface LiveTurn {
  segments: TurnSegment[]
  /**
   * 最後の項目が、続きの届きうる思考か本文か。ラウンドの区切り(`done`)とツールの実行で閉じる。
   * 閉じたあとに届いた思考・本文は、次のラウンドの新しい項目にする。
   */
  open: 'reasoning' | 'text' | null
}

export const NO_LIVE_TURN: LiveTurn = { segments: [], open: null }

/** 思考・ツールの項目を、末尾の折りたたみに足す(末尾が本文なら新しい折りたたみを開く)。 */
function withThought(segments: TurnSegment[], item: ThoughtItem): TurnSegment[] {
  const last = segments[segments.length - 1]
  if (last?.kind === 'thoughts') {
    return [...segments.slice(0, -1), { kind: 'thoughts', items: [...last.items, item] }]
  }
  return [...segments, { kind: 'thoughts', items: [item] }]
}

/**
 * 届いたイベントを1件積む。保存済みのターンと同じく、1ラウンドの思考と本文はそれぞれ1項目に
 * まとめる。実行前のツール呼び出しは描かず、実行の知らせ(`tool_executed`)で結果と一緒に出す。
 *
 * 受け取りの途中で失敗したラウンドの本文も流れたまま見えるが、保存はされない(断片なので。
 * `docs/spec/principles.md` 3節)。完了後に読み直した返信に置き換わる。
 */
export function appendTurnEvent(live: LiveTurn, event: TurnEvent): LiveTurn {
  const { segments, open } = live
  if (event.type === 'tool_executed') {
    const item: ThoughtItem = { kind: 'tool', id: event.id, execution: event.execution }
    return { segments: withThought(segments, item), open: null }
  }
  const response = event.event
  const last = segments[segments.length - 1]
  switch (response.type) {
    case 'reasoning_delta': {
      const lastItem = last?.kind === 'thoughts' ? last.items[last.items.length - 1] : undefined
      if (open === 'reasoning' && last?.kind === 'thoughts' && lastItem?.kind === 'reasoning') {
        const merged: ThoughtItem = { ...lastItem, text: lastItem.text + response.text }
        const thoughts: TurnSegment = { kind: 'thoughts', items: [...last.items.slice(0, -1), merged] }
        return { segments: [...segments.slice(0, -1), thoughts], open }
      }
      // 描画のキー。届いた思考の数で振る(ツールの項目は記録のidをキーにするので重ならない)。
      const count = segments.reduce(
        (n, segment) =>
          n + (segment.kind === 'thoughts' ? segment.items.filter((i) => i.kind === 'reasoning').length : 0),
        0,
      )
      const item: ThoughtItem = { kind: 'reasoning', id: count, text: response.text }
      return { segments: withThought(segments, item), open: 'reasoning' }
    }
    case 'text_delta': {
      if (open === 'text' && last?.kind === 'text') {
        const merged: TurnSegment = { ...last, text: last.text + response.text }
        return { segments: [...segments.slice(0, -1), merged], open }
      }
      const opened: TurnSegment = { kind: 'text', id: segments.length, text: response.text }
      return { segments: [...segments, opened], open: 'text' }
    }
    case 'done':
      return { ...live, open: null }
    case 'tool_call':
      return live
  }
}
