package net.niigo.scitl

import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import java.lang.ref.WeakReference

/**
 * カメラでQRを読む(docs/sync/index.md 11節)。Rust側(scitl_core::qr_scan)が[start]で読み取りの
 * 画面(ScanActivity)を開き、[poll]で結果を引き取り、やめるときは[cancel]で閉じる。読み取りは
 * WebViewを通さない。
 *
 * 読み取りは1度に1つで、新しく始めると前のものは閉じて取りやめとして扱う。
 */
object QrScanner {
  // 結果の頭の1文字。scitl_core::qr_scanの値と揃える(scitl-tauriのテストが照合する)。
  const val SCANNED = "S"
  const val CANCELLED = "C"
  const val DENIED = "D"
  const val FAILED = "F"

  const val EXTRA_ID = "id"

  private var current = 0L
  private var outcome: String? = null
  // 結果を置いたか(引き取ったあとも、後から届いた結果で上書きさせない)。
  private var settled = false
  private var screen: WeakReference<ScanActivity>? = null

  /** カメラがあるか(Rust側からJNIで呼ぶ)。無ければ、画面は読むボタンを出さない。 */
  @JvmStatic
  fun available(context: Context): Boolean =
    context.packageManager.hasSystemFeature(PackageManager.FEATURE_CAMERA_ANY)

  /** 読み取りの画面を開き、結果を引き取るための番号を返す(Rust側からJNIで呼ぶ)。 */
  @JvmStatic
  fun start(context: Context): Long {
    val id = synchronized(this) {
      current += 1
      outcome = null
      settled = false
      current
    }
    closeScreen()
    context.startActivity(
      Intent(context, ScanActivity::class.java)
        .putExtra(EXTRA_ID, id)
        // ApplicationのContextから開くので要る。同じアプリのタスク(MainActivityの上)に重なる。
        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    )
    return id
  }

  /**
   * 結果を引き取る(Rust側からJNIで呼ぶ)。まだ読んでいなければnull。結果は頭の1文字が種類で、
   * SCANNEDなら続きが読んだ中身、FAILEDなら続きが診断の文。引き取った結果は消す。
   */
  @JvmStatic
  fun poll(id: Long): String? = synchronized(this) {
    if (id != current) return CANCELLED
    outcome.also { outcome = null }
  }

  /**
   * 読み取りをやめ、画面が開いていれば閉じる(Rust側からJNIで呼ぶ)。結果が出たあとも閉じる(作り直された
   * 側の画面が残っていることがある)。
   */
  @JvmStatic
  fun cancel(id: Long) {
    synchronized(this) {
      if (id != current) return
      settled = true
    }
    closeScreen()
  }

  /** ScanActivityが開いたことを知らせる。番号が今の読み取りのものでなければ、閉じさせる(false)。 */
  fun attach(id: Long, activity: ScanActivity): Boolean = synchronized(this) {
    if (id != current || settled) return false
    screen = WeakReference(activity)
    true
  }

  /** ScanActivityが結果を置く。最初の1つだけを取る。 */
  fun finish(id: Long, result: String) {
    synchronized(this) {
      if (id == current && !settled) {
        outcome = result
        settled = true
      }
    }
  }

  private fun closeScreen() {
    val activity = synchronized(this) { screen?.get().also { screen = null } } ?: return
    activity.runOnUiThread { activity.finish() }
  }
}
