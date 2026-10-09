package net.niigo.scitl

import android.os.Bundle
import androidx.activity.OnBackPressedCallback
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    // 「戻る」で閉じるものが無いときは、アプリを終えずに裏へ回す(入力欄の下書き等を残す)。
    // 先に足した受け手ほど後に呼ばれるので、Tauri本体とwryの受け手(画面へ知らせる・WebViewの履歴を
    // 戻る)より先に足す。どちらも扱わないと、既定ではアクティビティが閉じてプロセスごと終わる。
    onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
      override fun handleOnBackPressed() {
        moveTaskToBack(true)
      }
    })
    super.onCreate(savedInstanceState)
  }
}
