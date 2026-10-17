package net.niigo.scitl

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Person
import android.content.Context
import android.content.Intent

/**
 * 利用者がアプリを見ていない間に終わった応答を、通知で知らせる
 * (docs/spec/architecture/concurrency.md「Androidで裏へ回ったとき」)。
 *
 * いつ終わったか・何を載せるかはRust側が決め(scitl_core::reply_notification)、文面は通知へ出す形に
 * 整えてから渡してくる。ここにあるのは、アプリが画面に出ているか(出ていれば出さない)の判定と、
 * 通知の出し方(チャンネル、同じ会話の通知の置き換え、押したときに開くもの)。
 */
object ReplyNotifier {
  /**
   * Rust側からJNIで呼ぶ。chatは通知を押したときに開く会話で、MainActivityのCHAT_GENERALか
   * タスクのID。同じ会話の通知は1つにまとめ、新しいもので置き換える。senderがあれば、
   * メッセージアプリと同じチャットの形にして、送り手と会話の名前(title)を両方出す。
   */
  @JvmStatic
  fun post(
    context: Context,
    chat: Long,
    title: String,
    sender: String?,
    body: String,
    userName: String,
    channelName: String
  ) {
    if (MainActivity.visible) return
    // 許可が無い間は、頼んでも捨てられる。チャンネルも作らないでおく。
    if (!Permissions.notificationsAllowed(context)) return
    val manager = context.getSystemService(NotificationManager::class.java)
    // 同じIDで作り直すと、名前だけが今の表示言語に更新される。生成中の通知(GeneratingService)とは
    // 別のチャンネルにして、利用者がOSの設定で片方だけを切れるようにする。
    manager.createNotificationChannel(
      NotificationChannel(CHANNEL_ID, channelName, NotificationManager.IMPORTANCE_DEFAULT)
    )
    val open = Intent(context, MainActivity::class.java)
      .putExtra(MainActivity.EXTRA_CHAT, chat)
      // extraだけが違うIntentは同じPendingIntentにまとめられ、後の会話で上書きされる。
      .setIdentifier(chat.toString())
    val builder = Notification.Builder(context, CHANNEL_ID)
      .setSmallIcon(R.drawable.ic_stat_generating)
    if (sender == null) {
      builder
        .setContentTitle(title)
        .setContentText(body)
        .setStyle(Notification.BigTextStyle().bigText(body))
    } else {
      // 1対1でも会話の名前を出すには、グループの会話にする必要がある。
      val style = Notification.MessagingStyle(Person.Builder().setName(userName).build())
        .setConversationTitle(title)
        .setGroupConversation(true)
        .addMessage(
          Notification.MessagingStyle.Message(
            body,
            System.currentTimeMillis(),
            Person.Builder().setName(sender).build()
          )
        )
      builder.setStyle(style)
    }
    val notification = builder
      .setContentIntent(
        PendingIntent.getActivity(
          context,
          0,
          open,
          PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
        )
      )
      .setAutoCancel(true)
      // 本文はモデルの出力。OSが文面から操作(リンクを開く等)を作って通知に足すと、アプリの
      // 「開く前の確認」を通らずに開けてしまう。
      .setAllowSystemGeneratedContextualActions(false)
      .build()
    manager.notify(chat.toString(), NOTIFICATION_ID, notification)
  }

  private const val CHANNEL_ID = "replies"
  // GeneratingServiceの通知(1)と重ねない。
  private const val NOTIFICATION_ID = 2
}
