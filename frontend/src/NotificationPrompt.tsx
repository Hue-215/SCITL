import { useEffect, useState } from 'react'
import { answerNotificationPrompt, notificationPromptDue } from './api'
import { ChoiceDialog } from './Dialog'
import { t } from './i18n'

// 通知の許可を求める前の説明。画面を読み込んだときに出す。ボタンを押さずに閉じたときは
// 答えたことにせず、次に読み込んだときにまた出る。
export default function NotificationPrompt() {
  const [open, setOpen] = useState(false)
  useEffect(() => {
    notificationPromptDue()
      .then(setOpen)
      .catch(() => undefined)
  }, [])
  if (!open) return null
  const answer = (allow: boolean) => {
    setOpen(false)
    answerNotificationPrompt(allow).catch(() => undefined)
  }
  return (
    <ChoiceDialog
      title={t('notification_prompt.title')}
      message={t('notification_prompt.message')}
      cancelLabel={t('notification_prompt.deny')}
      confirmLabel={t('common.continue')}
      confirmClass="primary"
      onConfirm={() => answer(true)}
      onCancel={() => answer(false)}
      onClose={() => setOpen(false)}
    />
  )
}
