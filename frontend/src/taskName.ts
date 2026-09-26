import { t } from './i18n'

// タイトル未設定のタスクを画面で何と呼ぶかを決める唯一の場所。サイドバーとヘッダーで
// 呼び方を食い違わせないため、両方ともここを通す。
// タイトルはモデルの update_task 頼みなので、付くまでは最初のユーザー発言で代用する
// (Issue #61。フォールバックの中身はRust側が作る)。
export function taskName(task: { title: string | null; fallback_label: string | null }): string {
  return task.title ?? task.fallback_label ?? t('task.untitled')
}
