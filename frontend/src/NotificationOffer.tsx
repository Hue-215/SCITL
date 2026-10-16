// 通知の許可を、OSのダイアログの前に説明する(Android)。出すかと時機(初めて応答を生成し終えたとき)は
// Rust側が決めて知らせてくる。答えると、以後は出ない。
import { useEffect, useState } from 'react'
import { answerNotificationOffer, watchNotificationOffers } from './api'
import Dialog from './Dialog'
import { t } from './i18n'

export default function NotificationOffer() {
  const [open, setOpen] = useState(false)
  useEffect(() => {
    // 知らせ先は1つで、渡し直すと置き換わる。
    watchNotificationOffers(() => setOpen(true)).catch(() => undefined)
  }, [])

  if (!open) return null
  // 閉じたときも断ったことにする。
  const answer = (accept: boolean) => {
    setOpen(false)
    void answerNotificationOffer(accept)
  }
  return (
    <Dialog title={t('notification_offer.title')} onClose={() => answer(false)}>
      <p>{t('notification_offer.message')}</p>
      <div className="button-row dialog-actions">
        <button type="button" onClick={() => answer(false)}>
          {t('notification_offer.decline')}
        </button>
        <button type="button" className="primary" onClick={() => answer(true)}>
          {t('common.continue')}
        </button>
      </div>
    </Dialog>
  )
}
