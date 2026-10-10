package net.niigo.scitl

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.IBinder
import android.util.Log

/**
 * 応答の生成中だけ動かすフォアグラウンドサービス。動いている間、裏へ回ったアプリのプロセスを
 * OSが止めにくくなる(docs/spec/architecture/concurrency.md「Androidで裏へ回ったとき」)。
 *
 * 仕事は持たない。始める・止める時機はRust側が決め(scitl_core::foreground_service)、
 * 通知の文面もRust側から受け取る。
 */
class GeneratingService : Service() {
  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    try {
      startForeground(
        NOTIFICATION_ID,
        notification(intent),
        ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC
      )
    } catch (e: Exception) {
      // アプリが前に出ていないときや、裏で動ける時間(下のonTimeout)を使い切っているときは、OSが断る。
      // 応答の生成は続くが、裏へ回ると止まりうる。
      Log.w(TAG, "could not enter the foreground", e)
      stopSelf()
    }
    // プロセスごと終わらされたら、生成も終わっている。サービスだけを作り直させない。
    return START_NOT_STICKY
  }

  /**
   * dataSyncが裏で動ける時間(24時間で合計6時間)を使い切ったときに、Android 15以上で呼ばれる。
   * 数秒以内に止めないとアプリごと落とされる。応答の生成は続くが、裏では止まりうる。
   */
  override fun onTimeout(startId: Int, fgsType: Int) {
    stopSelf()
  }

  private fun notification(intent: Intent?): Notification {
    val title = intent?.getStringExtra(EXTRA_TITLE) ?: getString(R.string.app_name)
    val channelName = intent?.getStringExtra(EXTRA_CHANNEL) ?: getString(R.string.app_name)
    // 同じIDで作り直すと、名前だけが今の表示言語に更新される。音は鳴らさない。
    getSystemService(NotificationManager::class.java).createNotificationChannel(
      NotificationChannel(CHANNEL_ID, channelName, NotificationManager.IMPORTANCE_LOW)
    )
    val open = PendingIntent.getActivity(
      this,
      0,
      Intent(this, MainActivity::class.java),
      PendingIntent.FLAG_IMMUTABLE
    )
    return Notification.Builder(this, CHANNEL_ID)
      .setSmallIcon(R.drawable.ic_stat_generating)
      .setContentTitle(title)
      .setContentIntent(open)
      .setOngoing(true)
      .build()
  }

  private companion object {
    const val TAG = "SCITL"
    const val CHANNEL_ID = "generating"
    const val NOTIFICATION_ID = 1
    // scitl_core::foreground_serviceが付ける名前と揃える。
    const val EXTRA_TITLE = "title"
    const val EXTRA_CHANNEL = "channel"
  }
}
