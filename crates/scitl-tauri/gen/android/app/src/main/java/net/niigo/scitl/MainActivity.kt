package net.niigo.scitl

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.activity.enableEdgeToEdge

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

    @Volatile
    private var requestedChat = CHAT_NONE

    private fun keepRequestedChat(intent: Intent?) {
      if (intent?.hasExtra(EXTRA_CHAT) == true) {
        requestedChat = intent.getLongExtra(EXTRA_CHAT, CHAT_NONE)
      }
    }

    /** 通知を押して届いた、開く会話の頼みを引き取る(Rust側からJNIで呼ぶ)。無ければCHAT_NONE。 */
    @JvmStatic
    @Synchronized
    fun takeRequestedChat(): Long {
      val chat = requestedChat
      requestedChat = CHAT_NONE
      return chat
    }

    /**
     * 応答の生成を始めたときに、通知の許可をまだ求めていなければ求める(GeneratingServiceが呼ぶ)。
     * 生成は利用者の操作で始まるので、このときActivityは前に出ている。
     */
    fun requestNotificationPermissionOnce() {
      current?.askForNotificationsOnce()
    }
  }
}
