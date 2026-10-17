import { useEffect, useState } from 'react'
import { answerNotificationPrompt, watchNotificationPrompt } from './api'
import Dialog from './Dialog'
import { t } from './i18n'

// 通知の許可を求める前の説明。Rust側が応答生成の終わりに知らせてきたら出す。ボタンを押さずに
// 閉じたときは答えたことにせず、次の応答生成の終わりにまた出る。
export default function NotificationPrompt() {
  const [open, setOpen] = useState(false)
  useEffect(() => {
    // 知らせ先は1つで、渡し直すと置き換わる。
    watchNotificationPrompt(() => setOpen(true)).catch(() => undefined)
  }, [])
  if (!open) return null
  const answer = (allow: boolean) => {
    setOpen(false)
    answerNotificationPrompt(allow).catch(() => undefined)
  }
  return (
    <Dialog title={t('notification_prompt.title')} onClose={() => setOpen(false)}>
      <p>{t('notification_prompt.message')}</p>
      <div className="button-row dialog-actions">
        <button type="button" onClick={() => answer(false)}>
          {t('notification_prompt.deny')}
        </button>
        <button type="button" className="primary" onClick={() => answer(true)}>
          {t('common.continue')}
        </button>
      </div>
    </Dialog>
  )
}
