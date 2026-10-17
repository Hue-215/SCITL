package net.niigo.scitl

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.provider.Settings

/**
 * カメラと通知の許可を求める前に、画面で説明を挟むための状態と操作(scitl_core::permissions)。
 * 説明を出すかはRust側が決める。ここは許可の状態を答え、OSのダイアログと設定アプリを開く。
 *
 * 乗っ取った画面から何度呼ばれても、起きるのはOSのダイアログか、このアプリの設定画面が開くことだけ。
 */
object Permissions {
  // scitl_core::permissionsの値と揃える(scitl-tauriのテストが照合する)。
  const val GRANTED = 0
  const val ASK = 1
  const val BLOCKED = 2

  /**
   * カメラの許可の状態(Rust側からJNIで呼ぶ)。shouldShowRequestPermissionRationaleは、1回も求めて
   * いないときと、OSがもうダイアログを出さないときの両方でfalseを返すので、求めたかの記録と合わせる。
   * OSのダイアログを選ばずに閉じたときもfalseのままなので、BLOCKEDになる(設定アプリでは許可できる)。
   * Activityが無くて確かめられないときは、ASKに倒す(OSのダイアログは読み取りの画面が出す)。
   */
  @JvmStatic
  fun camera(context: Context): Int {
    val camera = Manifest.permission.CAMERA
    if (context.checkSelfPermission(camera) == PackageManager.PERMISSION_GRANTED) return GRANTED
    val activity = MainActivity.current ?: return ASK
    if (activity.shouldShowRequestPermissionRationale(camera)) return ASK
    return if (asked(context, CAMERA_PREFERENCES)) BLOCKED else ASK
  }

  /** ScanActivityがOSのダイアログを出す前に呼ぶ。 */
  fun markCameraAsked(context: Context) {
    markAsked(context, CAMERA_PREFERENCES)
  }

  /** このアプリの設定画面を開く(Rust側からJNIで呼ぶ)。 */
  @JvmStatic
  fun openSettings(context: Context) {
    context.startActivity(
      Intent(
        Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
        Uri.fromParts("package", context.packageName, null)
      ).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    )
  }

  /**
   * 通知の許可を求める説明を出すか(Rust側からJNIで呼ぶ)。Android 13以上で、許可が無く、
   * まだ求めていないとき。
   */
  @JvmStatic
  fun notificationPromptDue(context: Context): Boolean =
    !notificationsAllowed(context) && !asked(context, NOTIFICATION_PREFERENCES)

  /** 通知を出せるか(Android 12以下は許可が要らない)。 */
  fun notificationsAllowed(context: Context): Boolean =
    Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
      context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) ==
      PackageManager.PERMISSION_GRANTED

  /**
   * 説明への答え(Rust側からJNIで呼ぶ)。どちらでも求めたことにし、以後は説明を出さない。
   * allowならOSのダイアログを出す。出せるActivityが無ければ求めたことにせず、次の機会にまた説明を出す。
   */
  @JvmStatic
  fun answerNotificationPrompt(context: Context, allow: Boolean) {
    if (!notificationPromptDue(context)) return
    if (!allow) {
      markAsked(context, NOTIFICATION_PREFERENCES)
      return
    }
    val activity = MainActivity.current ?: throw IllegalStateException("no activity to ask from")
    markAsked(context, NOTIFICATION_PREFERENCES)
    activity.runOnUiThread {
      activity.requestPermissions(
        arrayOf(Manifest.permission.POST_NOTIFICATIONS),
        REQUEST_NOTIFICATIONS
      )
    }
  }

  private fun asked(context: Context, preferences: String): Boolean =
    context.getSharedPreferences(preferences, Context.MODE_PRIVATE).getBoolean(KEY_ASKED, false)

  private fun markAsked(context: Context, preferences: String) {
    context.getSharedPreferences(preferences, Context.MODE_PRIVATE).edit()
      .putBoolean(KEY_ASKED, true)
      .apply()
  }

  // 通知の記録は、説明を挟む前の版がOSのダイアログを直接出したときと同じ場所に持つ(その版で
  // 求めた端末で、もう一度求めないため)。
  private const val NOTIFICATION_PREFERENCES = "notifications"
  private const val CAMERA_PREFERENCES = "camera"
  private const val KEY_ASKED = "permission_asked"
  private const val REQUEST_NOTIFICATIONS = 1
}
