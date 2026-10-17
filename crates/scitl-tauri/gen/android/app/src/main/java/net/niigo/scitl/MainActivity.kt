package net.niigo.scitl

import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge
import java.lang.ref.WeakReference
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

  /**
   * 画面からのカメラ・マイク等の要求を断るクライアントで包む(GuardedChromeClient)。wryは、この呼び出しと
   * 同じ処理の中で、ページを読み込み始めたあとにクライアントを付ける。そのため、メインスレッドの次の回へ
   * 回して包む(その時点では、ページはまだ読み込まれておらず、画面から権限を求められることは無い)。
   * wryの更新で順序が変わったときに守りが黙って外れないよう、まだ付いていなければ少し待って確かめ直し、
   * 前に戻るたびにも確かめる。付いていない間はWebViewの既定で要求は断られるが、待つ間にwryが付けると、
   * 次に確かめるまでの1回分の間隔はwryのクライアントのまま働く。
   */
  override fun onWebViewCreate(webView: WebView) {
    super.onWebViewCreate(webView)
    this.webView = WeakReference(webView)
    Handler(Looper.getMainLooper()).post { guard(webView) }
  }

  override fun onResume() {
    super.onResume()
    webView?.get()?.let { guard(it) }
  }

  private var webView: WeakReference<WebView>? = null

  private fun guard(webView: WebView, retriesLeft: Int = GUARD_RETRIES) {
    val inner = webView.webChromeClient
    if (inner is GuardedChromeClient) return
    if (inner == null) {
      // wryがまだクライアントを付けていない(順序が変わった)。包んでも上書きされるので、付くまで待つ。
      // 待っても付かなければ、気付けるよう診断に出す(次に前に戻ったときにも包み直す)。
      if (retriesLeft > 0) {
        Handler(Looper.getMainLooper()).postDelayed({ guard(webView, retriesLeft - 1) }, GUARD_RETRY_MS)
      } else {
        Log.e("SCITL", "the WebChromeClient is not set yet; camera requests from the page are not guarded")
      }
      return
    }
    webView.webChromeClient = GuardedChromeClient(inner)
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

    // クライアントが付くのを待つ回数と間隔(GuardedChromeClientで包むとき)。
    private const val GUARD_RETRIES = 40
    private const val GUARD_RETRY_MS = 50L

    /** アプリが画面に出ているか。出ている間に終わった応答は、通知しない(ReplyNotifier)。 */
    @Volatile
    @JvmStatic
    var visible = false
      private set

    /** 許可を求めるとき(Permissions)に使う。書くのはメインスレッド、読むのはRust側のスレッドもある。 */
    @Volatile
    internal var current: MainActivity? = null
      private set

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
  }
}
