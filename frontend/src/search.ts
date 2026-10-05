// 検索欄の語で一覧を絞り込む(前後の空白を無視し、大文字小文字を区別しない部分一致)。
// 検索欄のある一覧(モデル表・取得したモデルの候補)は、すべてこれを通して
// 同じ規則で絞り込む。`searching`は語が空でないか(空なら`matched`は`items`そのもの)。
export function matchQuery<T>(
  items: T[],
  query: string,
  text: (item: T) => string,
): { matched: T[]; searching: boolean } {
  const needle = query.trim().toLowerCase()
  if (!needle) return { matched: items, searching: false }
  return {
    matched: items.filter((item) => text(item).toLowerCase().includes(needle)),
    searching: true,
  }
}
