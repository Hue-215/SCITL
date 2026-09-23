import type { Html, Image, ImageReference, Nodes, Parent, PhrasingContent, Root } from 'mdast'

// Markdownの構文木から、描画時に外部へのアクセスや生のHTML解釈を起こしうる要素を
// 取り除く remark プラグイン(principles.md 4節「画像の自動取得を防ぐ」)。
//
// - 画像記法(`![alt](url)`・参照形式)は、画像と分かるラベルを付けたリンクに変える。
//   リンクは確認ダイアログを経てしか開けない(Markdown.tsx)
// - 生のHTMLは解釈しない。`<img>`は画像記法と同じくリンクに、`<br>`は改行に変え、
//   それ以外のタグは書かれた文字列のまま表示する(黙って消すと、モデルが何を書いたかが
//   見えなくなるため)
//
// 描画側(react-markdown)は元々HTMLを解釈しない設定で使うが、画像はHTMLを経由せずに
// 描画されるため、ここで構文木の段階で潰す。CSPの img-src と合わせた多層防御。

const IMAGE_FALLBACK_LABEL = '画像'
const RAW_TAG = /<img\b[^>]*>|<br\s*\/?>/gi

function imageLabel(alt: string | null | undefined): string {
  const text = alt?.trim()
  return `🖼 ${text || IMAGE_FALLBACK_LABEL}`
}

function htmlAttr(tag: string, name: string): string | undefined {
  const m = tag.match(new RegExp(`\\b${name}\\s*=\\s*(?:"([^"]*)"|'([^']*)'|([^\\s"'>]+))`, 'i'))
  return m ? (m[1] ?? m[2] ?? m[3]) : undefined
}

// リンクの中に画像があった場合(`[![alt](img)](href)`)はリンクを入れ子にできないため、
// ラベルの文字列だけを残す。
function imageToPhrasing(src: string | undefined, alt: string | undefined, inLink: boolean): PhrasingContent {
  const label = { type: 'text' as const, value: imageLabel(alt) }
  if (inLink || !src) return label
  return { type: 'link', url: src, title: null, children: [label] }
}

function htmlToPhrasing(node: Html, inLink: boolean): PhrasingContent[] {
  const out: PhrasingContent[] = []
  let last = 0
  for (const m of node.value.matchAll(RAW_TAG)) {
    if (m.index > last) out.push({ type: 'text', value: node.value.slice(last, m.index) })
    const tag = m[0]
    if (/^<br/i.test(tag)) {
      out.push({ type: 'break' })
    } else {
      out.push(imageToPhrasing(htmlAttr(tag, 'src'), htmlAttr(tag, 'alt'), inLink))
    }
    last = m.index + tag.length
  }
  if (last < node.value.length) out.push({ type: 'text', value: node.value.slice(last) })
  return out
}

function imageReferenceToPhrasing(node: ImageReference, inLink: boolean): PhrasingContent {
  const label = { type: 'text' as const, value: imageLabel(node.alt) }
  if (inLink) return label
  return {
    type: 'linkReference',
    identifier: node.identifier,
    label: node.label,
    referenceType: node.referenceType,
    children: [label],
  }
}

const PHRASING_PARENTS = new Set([
  'paragraph',
  'heading',
  'emphasis',
  'strong',
  'delete',
  'link',
  'linkReference',
  'tableCell',
])

function transform(parent: Parent, inLink: boolean): void {
  const phrasing = PHRASING_PARENTS.has(parent.type)
  const children: Nodes[] = []
  for (const child of parent.children as Nodes[]) {
    let replaced: PhrasingContent[] | null = null
    if (child.type === 'html') {
      replaced = htmlToPhrasing(child, inLink)
    } else if (child.type === 'image') {
      const image: Image = child
      replaced = [imageToPhrasing(image.url, image.alt ?? undefined, inLink)]
    } else if (child.type === 'imageReference') {
      replaced = [imageReferenceToPhrasing(child, inLink)]
    }

    if (replaced === null) {
      if ('children' in child) {
        transform(child, inLink || child.type === 'link' || child.type === 'linkReference')
      }
      children.push(child)
    } else if (phrasing) {
      children.push(...replaced)
    } else if (replaced.length > 0) {
      // ブロックとして書かれたHTML(行頭の`<img>`等)は段落に包んで置く
      children.push({ type: 'paragraph', children: replaced })
    }
  }
  parent.children = children as Parent['children']
}

export default function remarkInertHtml() {
  return (tree: Root) => transform(tree, false)
}
