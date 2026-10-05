import { useState } from 'react'

// この件数以上の一覧は既定で畳む。モデル表とMCPのツール一覧で揃える。
const LIST_COLLAPSE_THRESHOLD = 5

// 件数の多い一覧を既定で畳む状態。`collapsible`は畳める件数か、`collapsed`は今畳んでいるか。
// `toggle`は`CollapseToggle`にそのまま渡す。
export function useCollapse(count: number) {
  const [expanded, setExpanded] = useState(false)
  const collapsible = count >= LIST_COLLAPSE_THRESHOLD
  return {
    collapsible,
    collapsed: collapsible && !expanded,
    toggle: { expanded, onToggle: () => setExpanded((v) => !v) },
  }
}
