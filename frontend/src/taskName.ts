import { t } from './i18n'

// タイトル未設定のタスクを画面で何と呼ぶかを決める唯一の場所。サイドバーとヘッダーで
// 呼び方を食い違わせないため、両方ともここを通す。タイトルはモデルの update_task 頼みなので、
// 付くまでは最初のユーザー発言で代用する(フォールバックの中身はRust側が作る)。
export function taskName(task: { title: string | null; fallback_label: string | null }): string {
  return task.title ?? task.fallback_label ?? t('task.untitled')
}

// 締切と工程の進捗の書き方。サイドバーとヘッダーで食い違わせないため、両方ともここを通す。
// 並べ方(区切り)はそれぞれの文言が決める。
export function taskProgress(task: {
  deadline: string | null
  steps_done: number
  steps_total: number
}): { deadline: string; steps: string } {
  return {
    deadline: t('task.deadline', { deadline: task.deadline ?? t('task.deadline_unset') }),
    steps:
      task.steps_total > 0
        ? t('task.steps_progress', { done: task.steps_done, total: task.steps_total })
        : t('task.steps_none'),
  }
}
