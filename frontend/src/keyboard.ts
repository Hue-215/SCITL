import type { KeyboardEvent } from 'react'

// IME変換を確定するためのEnterではない、確定操作としてのEnterか。isComposingが正しく
// 立たない古いWebKitGTKもあるため、keyCode 229(IME処理中を示す慣習値)も併せて見る。
export function isCommitEnter(e: KeyboardEvent): boolean {
  return e.key === 'Enter' && !e.nativeEvent.isComposing && e.keyCode !== 229
}

// フォーカスを受け取れる要素。ダイアログのフォーカストラップと、引き出し(useDrawer.ts)を開いたときの
// フォーカスの移し先を探すのに使う。
export const FOCUSABLE_SELECTOR =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])'
