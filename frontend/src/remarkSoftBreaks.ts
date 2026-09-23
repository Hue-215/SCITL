import type { Nodes, Parent, PhrasingContent, Root } from 'mdast'

// 段落中の単独の改行(Markdownでは空白に畳まれる)を、そのまま改行として描画する
// remark プラグイン。チャットの発言は入力欄での改行(Shift+Enter)をそのまま見せる方が
// 自然なため、Markdownの規則より書いたままの見た目を優先する。

function splitText(value: string, afterBreak: boolean): PhrasingContent[] {
  const out: PhrasingContent[] = []
  value.split('\n').forEach((line, i) => {
    // `<br>`の直後の改行は、`<br>`自体が改行済みなので二重にしない
    if (i > 0 && !(i === 1 && afterBreak && out.length === 0)) out.push({ type: 'break' })
    if (line) out.push({ type: 'text', value: line })
  })
  return out
}

function transform(parent: Parent): void {
  const children: Nodes[] = []
  for (const child of parent.children as Nodes[]) {
    if (child.type === 'text' && child.value.includes('\n')) {
      children.push(...splitText(child.value, children.at(-1)?.type === 'break'))
    } else {
      if ('children' in child) transform(child)
      children.push(child)
    }
  }
  parent.children = children as Parent['children']
}

export default function remarkSoftBreaks() {
  return (tree: Root) => transform(tree)
}
