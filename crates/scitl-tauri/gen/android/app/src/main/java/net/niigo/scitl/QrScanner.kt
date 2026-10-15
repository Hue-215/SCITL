package net.niigo.scitl

import android.content.Context
import android.content.Intent

/**
 * カメラでQRを読む(docs/sync/index.md 11節)。Rust側(scitl_core::qr_scan)が[start]で読み取りの
 * 画面(ScanActivity)を開き、[poll]で結果を引き取る。読み取りはWebViewを通さない。
 *
 * 読み取りは1度に1つで、新しく始めると前のものは取りやめとして扱う。
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

  /** 読み取りの画面を開き、結果を引き取るための番号を返す(Rust側からJNIで呼ぶ)。 */
  @JvmStatic
  fun start(context: Context): Long {
    val id = synchronized(this) {
      current += 1
      outcome = null
      current
    }
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
   * SCANNEDなら続きが読んだ中身、FAILEDなら続きが診断の文。
   */
  @JvmStatic
  fun poll(id: Long): String? = synchronized(this) {
    if (id == current) outcome else CANCELLED
  }

  /** ScanActivityが結果を置く。最初の1つだけを取る。 */
  fun finish(id: Long, result: String) {
    synchronized(this) {
      if (id == current && outcome == null) outcome = result
    }
  }
}
