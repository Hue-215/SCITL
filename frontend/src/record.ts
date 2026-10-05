/**
 * `key`を除いた写し。`key`が無ければ同じオブジェクトを返す(stateの更新で使ったとき、
 * 変化が無いものとして扱われるように)。
 */
export function without<T>(map: Record<string, T>, key: string): Record<string, T> {
  if (!(key in map)) return map
  const next = { ...map }
  delete next[key]
  return next
}
