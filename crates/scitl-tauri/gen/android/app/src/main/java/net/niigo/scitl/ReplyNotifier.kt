package net.niigo.scitl

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Person
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build

/**
 * 利用者がアプリを見ていない間に終わった応答を、通知で知らせる
 * (docs/spec/architecture/concurrency.md「Androidで裏へ回ったとき」)。
 *
 * いつ終わったか・何を載せるかはRust側が決め(scitl_core::reply_notification)、文面は通知へ出す形に
 * 整えてから渡してくる。ここにあるのは、アプリが画面に出ているか(出ていれば出さない)の判定と、
 * 通知の出し方(チャンネル、同じ会話の通知の置き換え、押したときに開くもの)と、通知の許可。
 */
object ReplyNotifier {
  /**
   * Rust側からJNIで呼ぶ。chatは通知を押したときに開く会話で、MainActivityのCHAT_GENERALか
   * タスクのID。同じ会話の通知は1つにまとめ、新しいもので置き換える。senderがあれば、チャットの形
   * (送り手と会話の名前)で出す。userはその形でOSが求める利用者の名前で、利用者の発言は載せない。
   */
  @JvmStatic
  fun post(
    context: Context,
    chat: Long,
    title: String,
    sender: String?,
    user: String,
    body: String,
    channelName: String
  ) {
    if (MainActivity.visible) return
    // 許可が無い間は、頼んでも捨てられる。チャンネルも作らないでおく。
    if (!permitted(context)) return
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
    val style = if (sender != null) {
      // 会話の名前(題)は、グループの会話にしたときだけ出る。
      Notification.MessagingStyle(Person.Builder().setName(user).build())
        .setConversationTitle(title)
        .setGroupConversation(true)
        .addMessage(body, System.currentTimeMillis(), Person.Builder().setName(sender).build())
    } else {
      Notification.BigTextStyle().bigText(body)
    }
    val notification = Notification.Builder(context, CHANNEL_ID)
      .setSmallIcon(R.drawable.ic_stat_generating)
      .setContentTitle(title)
      .setContentText(body)
      .setStyle(style)
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

  /**
   * 通知の許可を、画面で説明してから求めるか(Rust側からJNIで呼ぶ)。Android 13以上で、許可が無く、
   * まだ求めていないとき。
   */
  @JvmStatic
  fun permissionNeeded(context: Context): Boolean =
    !permitted(context) && !preferences(context).getBoolean(KEY_ASKED, false)

  /**
   * 画面での説明に答えた(Rust側からJNIで呼ぶ)。求めるのは1回だけで、断られても(説明で断られても)
   * 以後は求めない(利用者はOSの設定で変えられる)。
   */
  @JvmStatic
  fun answerPermissionOffer(context: Context, accept: Boolean) {
    val preferences = preferences(context)
    if (preferences.getBoolean(KEY_ASKED, false)) return
    preferences.edit().putBoolean(KEY_ASKED, true).apply()
    if (accept && !permitted(context)) {
      MainActivity.request(Manifest.permission.POST_NOTIFICATIONS, REQUEST_PERMISSION)
    }
  }

  // 通知の許可が要るのはAndroid 13以上。
  private fun permitted(context: Context): Boolean =
    Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
      context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) ==
      PackageManager.PERMISSION_GRANTED

  private fun preferences(context: Context) =
    context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)

  // 名前を変えると、もう求めた端末でも、もう1回求めることになる。
  private const val PREFERENCES = "notifications"
  private const val KEY_ASKED = "permission_asked"
  private const val REQUEST_PERMISSION = 1

  private const val CHANNEL_ID = "replies"
  // GeneratingServiceの通知(1)と重ねない。
  private const val NOTIFICATION_ID = 2
}
