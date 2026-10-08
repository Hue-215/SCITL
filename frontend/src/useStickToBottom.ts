import { useCallback, useRef } from 'react'
import type { WheelEvent } from 'react'

// 会話欄を新しい発言に追従させる。下端にいる間だけ追従し、ユーザーが上へ
// スクロールしたらやめる。下端付近まで戻したら、また追従する。送信・編集・再試行を
// 始めたときとタスクを開いたときは、どこを見ていても下端へ戻す(`stick`)。
//
// 追従をやめるかどうかは、下端からの距離ではなく、上へ動かしたかどうかで決める。
// 距離で決めると、トラックパッドやスムーズスクロールの1回の動きが小さい間は「下端付近」に
// 留まり、ストリーミングの途中経過が届くたびに下端へ引き戻されて、上へ戻れない。

// 下へ戻したとき、これより下端に近ければ追従を再開する。ぴったり0にすると、小数の
// 座標で1px足りずに再開しないことがある。
const NEAR_BOTTOM_PX = 48
// これ以下の下端からの距離は「下端にいる」とみなす。中身が縮んで位置が上へ詰められた
// ときは、上へ動いても下端に留まるので、追従をやめない。
const AT_BOTTOM_PX = 1

export function useStickToBottom<T extends HTMLElement>() {
  const ref = useRef<T>(null)
  const stuck = useRef(true)
  // `stick`のあと、次の`follow`までは位置の判定をせずに下端へ戻す。会話欄が作り直された
  // (設定画面から戻った)直後は位置が先頭にあり、上へ動いたように見えてしまうため。
  const forced = useRef(true)
  // 前に見たスクロール位置。上へ動いたか下へ動いたかを、これと比べて決める。
  const lastTop = useRef(0)

  // 前に見た位置からの動きで、追従を続けるかを決める。スクロールイベントのほか、
  // `follow`が位置を動かす前にも呼ぶ。スクロールイベントは次の描画の機会まで遅れて
  // 届くので、その前に`follow`が下端へ戻すと、ユーザーが上へ動かした分が消えて
  // 気付けなくなる。
  const observe = useCallback((el: T) => {
    const top = el.scrollTop
    const distance = el.scrollHeight - top - el.clientHeight
    if (top < lastTop.current && distance > AT_BOTTOM_PX) stuck.current = false
    else if (top > lastTop.current && distance < NEAR_BOTTOM_PX) stuck.current = true
    lastTop.current = top
  }, [])

  const onScroll = useCallback(() => {
    if (ref.current !== null) observe(ref.current)
  }, [observe])

  // スムーズスクロールでは、ホイールを回してから位置が動き始めるまでの間に`follow`が
  // 下端へ戻すと、その動き自体が打ち消される。ホイールは動く前に届くので、上へ回した
  // 時点で追従をやめる。先頭にいて上へ動けないとき(中身が窓に収まっているときを含む)と、
  // Ctrlを押して拡大率を変えているときは動かないので、やめない。
  // 下端で下へ回したときは位置が動かずスクロールイベントも出ないので、ここで追従を
  // 再開する(追従をやめたあと、中身が縮んだり窓が広がったりして下端に詰められた場合)。
  const onWheel = useCallback((event: WheelEvent<T>) => {
    if (event.ctrlKey) return
    const el = event.currentTarget
    const distance = el.scrollHeight - el.scrollTop - el.clientHeight
    if (event.deltaY < 0 && el.scrollTop > 0) stuck.current = false
    else if (event.deltaY > 0 && distance <= AT_BOTTOM_PX) stuck.current = true
  }, [])

  /** 次に中身が変わったとき、スクロール位置によらず下端を見せる。 */
  const stick = useCallback(() => {
    stuck.current = true
    forced.current = true
  }, [])

  /**
   * 中身が変わったあとに呼ぶ。下端にいれば(または`stick`のあとなら)下端へ合わせる。
   * 描画後だと伸びた分だけ上にずれた状態が一瞬見えるので、`useLayoutEffect`から呼ぶ。
   */
  const follow = useCallback(() => {
    const el = ref.current
    if (el === null) return
    if (forced.current) forced.current = false
    else observe(el)
    if (!stuck.current) return
    el.scrollTop = el.scrollHeight
    // ここで動かした分は、ユーザーの動きとして判定しない。
    lastTop.current = el.scrollTop
  }, [observe])

  return { ref, onScroll, onWheel, stick, follow }
}
