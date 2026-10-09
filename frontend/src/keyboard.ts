import type { KeyboardEvent } from 'react'

// IME変換を確定するためのEnterではない、確定操作としてのEnterか。isComposingが正しく
// 立たない古いWebKitGTKもあるため、keyCode 229(IME処理中を示す慣習値)も併せて見る。
export function isCommitEnter(e: KeyboardEvent): boolean {
  return e.key === 'Enter' && !e.nativeEvent.isComposing && e.keyCode !== 229
}

// 主に指で操作する端末か。窓の幅(useDrawer.ts)と同じく端末の種類では分けず、主な入力の手段
// (指かマウスか)で決める。
const coarsePointer = window.matchMedia('(pointer: coarse)')

// チャットの入力欄で、Enterで送るか。指で操作する端末では、ソフトキーボードのEnterは
// 改行のつもりで押されるので改行にし、送るのはボタン(つないだキーボードならCtrl+Enterでも)にする。
// Shift+Enterはどの端末でも改行。
export function isSendEnter(e: KeyboardEvent): boolean {
  if (!isCommitEnter(e) || e.shiftKey) return false
  return !coarsePointer.matches || e.ctrlKey || e.metaKey
}

// フォーカスを受け取れる要素。ダイアログのフォーカストラップと、引き出し(useDrawer.ts)を開いたときの
// フォーカスの移し先を探すのに使う。
export const FOCUSABLE_SELECTOR =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])'
