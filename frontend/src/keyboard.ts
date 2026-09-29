import type { KeyboardEvent } from 'react'

// IME変換を確定するためのEnterではない、確定操作としてのEnterか。isComposingが正しく
// 立たない古いWebKitGTKもあるため、keyCode 229(IME処理中を示す慣習値)も併せて見る。
export function isCommitEnter(e: KeyboardEvent): boolean {
  return e.key === 'Enter' && !e.nativeEvent.isComposing && e.keyCode !== 229
}
