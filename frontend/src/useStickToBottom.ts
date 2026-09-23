import { useCallback, useRef } from 'react'

// 会話欄を新しい発言に追従させる(Issue #164)。下端付近にいる間だけ追従し、ユーザーが上へ
// スクロールして過去の発言を読んでいる間は動かさない。送信・編集・再試行を始めたときと
// タスクを開いたときは、どこを見ていても下端へ戻す(`stick`)。

// これより下端に近ければ「下端にいる」とみなす。ぴったり0にすると、小数の座標で
// 1px足りずに追従が外れることがある。
const NEAR_BOTTOM_PX = 48

export function useStickToBottom<T extends HTMLElement>() {
  const ref = useRef<T>(null)
  const stuck = useRef(true)

  const onScroll = useCallback(() => {
    const el = ref.current
    if (el === null) return
    stuck.current = el.scrollHeight - el.scrollTop - el.clientHeight < NEAR_BOTTOM_PX
  }, [])

  /** 次に中身が変わったとき、スクロール位置によらず下端を見せる。 */
  const stick = useCallback(() => {
    stuck.current = true
  }, [])

  /**
   * 中身が変わったあとに呼ぶ。下端にいれば(または`stick`のあとなら)下端へ合わせる。
   * 描画後だと伸びた分だけ上にずれた状態が一瞬見えるので、`useLayoutEffect`から呼ぶ。
   */
  const follow = useCallback(() => {
    const el = ref.current
    if (el !== null && stuck.current) el.scrollTop = el.scrollHeight
  }, [])

  return { ref, onScroll, stick, follow }
}
