package net.niigo.scitl

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.activity.enableEdgeToEdge
import java.util.concurrent.atomic.AtomicLong

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    current = this
    // 画面の向きを変えた等でActivityが作り直されたときは、同じIntentがもう一度届く。押したのは
    // 前の1回だけなので、頼みにしない。
    if (savedInstanceState == null) keepRequestedChat(intent)
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    keepRequestedChat(intent)
  }

  /**
   * 画面に閉じるものが無いときの「戻る」。Activityを閉じずに、アプリを裏へ回す。OSの既定は、
   * ホーム画面のアイコンから起動したタスクだけを裏へ回し、通知から起動したタスク等ではActivityを
   * 閉じてプロセスごと終える。生成中の応答が途中で止まり、入力欄の下書きも消えるため、起動の
   * しかたによらず揃える。
   *
   * 呼ぶのはTauri本体のAppPlugin(画面が「戻る」の受け手を登録しておらず、WebViewでも戻れないとき)
   * で、それがonBackPressed()を呼ぶ実装であることに頼っている。Tauriを更新したら、通知から起動して
   * 「戻る」でプロセスが残るかを確かめる(android-checkスキル)。
   */
  @Suppress("OVERRIDE_DEPRECATION")
  override fun onBackPressed() {
    moveTaskToBack(true)
  }

  override fun onStart() {
    super.onStart()
    visible = true
  }

  override fun onStop() {
    visible = false
    super.onStop()
  }

  override fun onDestroy() {
    if (current === this) current = null
    super.onDestroy()
  }

  /**
   * 通知の許可(Android 13以上)を、まだ求めていなければ求める。求めるのは1回だけで、断られたら
   * 以後は求めない(利用者はOSの設定で変えられる)。
   */
  private fun askForNotificationsOnce() {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
    if (checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) ==
      PackageManager.PERMISSION_GRANTED
    ) {
      return
    }
    val asked = getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
    if (asked.getBoolean(KEY_ASKED, false)) return
    asked.edit().putBoolean(KEY_ASKED, true).apply()
    requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), REQUEST_NOTIFICATIONS)
  }

  companion object {
    /**
     * 通知(ReplyNotifier)を押したときに開く会話を載せるextra。値はCHAT_GENERALかタスクのID。
     * このActivityはほかのアプリからも起動できるので、値はRust側が確かめてから使う
     * (scitl_core::reply_notification::resolve_requested_chat)。
     */
    const val EXTRA_CHAT = "chat"
    // scitl_core::reply_notificationの値と揃える(scitl-tauriのテストが照合する)。
    const val CHAT_GENERAL = 0L
    const val CHAT_NONE = -1L

    private const val PREFERENCES = "notifications"
    private const val KEY_ASKED = "permission_asked"
    private const val REQUEST_NOTIFICATIONS = 1

    /** アプリが画面に出ているか。出ている間に終わった応答は、通知しない(ReplyNotifier)。 */
    @Volatile
    @JvmStatic
    var visible = false
      private set

    private var current: MainActivity? = null

    // 書くのはメインスレッド、引き取るのはRust側のスレッド。
    private val requestedChat = AtomicLong(CHAT_NONE)

    private fun keepRequestedChat(intent: Intent?) {
      try {
        if (intent?.hasExtra(EXTRA_CHAT) == true) {
          requestedChat.set(intent.getLongExtra(EXTRA_CHAT, CHAT_NONE))
        }
      } catch (e: RuntimeException) {
        // ほかのアプリが、読めないextraを付けて起動した(Android 12以下は、extraを1つ読むだけで
        // 全部を展開する)。頼みは無かったことにする。
      }
    }

    /** 通知を押して届いた、開く会話の頼みを引き取る(Rust側からJNIで呼ぶ)。無ければCHAT_NONE。 */
    @JvmStatic
    fun takeRequestedChat(): Long = requestedChat.getAndSet(CHAT_NONE)

    /**
     * 応答の生成を始めたときに、通知の許可をまだ求めていなければ求める(GeneratingServiceが呼ぶ)。
     * 生成は利用者の操作で始まるので、このときActivityは前に出ている。
     */
    fun requestNotificationPermissionOnce() {
      current?.askForNotificationsOnce()
    }
  }
}
