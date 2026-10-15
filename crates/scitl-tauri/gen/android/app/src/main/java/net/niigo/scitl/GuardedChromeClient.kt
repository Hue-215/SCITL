package net.niigo.scitl

import android.graphics.Bitmap
import android.net.Uri
import android.os.Message
import android.view.View
import android.webkit.ConsoleMessage
import android.webkit.GeolocationPermissions
import android.webkit.JsPromptResult
import android.webkit.JsResult
import android.webkit.PermissionRequest
import android.webkit.ValueCallback
import android.webkit.WebChromeClient
import android.webkit.WebView

/**
 * wryのWebChromeClient(生成される`RustWebChromeClient`)を包み、画面(WebView)からのカメラ・マイク等の
 * 要求と位置情報の要求を常に断る。ほかはすべて中へ回す。
 *
 * wryは、アプリがAndroidの権限を持っていれば画面の要求をそのまま許す。アプリはQRを読むために
 * カメラの権限を持つ(ScanActivity)ので、包まないと、乗っ取られた画面が`getUserMedia`でカメラを
 * 使える(docs/spec/architecture/webview-boundary.md「画面が持つもの・持たないもの」)。
 *
 * 中のクラスはビルドのたびに生成され、書き換えられないので包む。中が上書きしていないメソッドも
 * 回すのは、wryが上書きを足しても黙って落とさないため。
 */
class GuardedChromeClient(private val inner: WebChromeClient) : WebChromeClient() {
  override fun onPermissionRequest(request: PermissionRequest) {
    request.deny()
  }

  override fun onGeolocationPermissionsShowPrompt(
    origin: String?,
    callback: GeolocationPermissions.Callback?
  ) {
    callback?.invoke(origin, false, false)
  }

  override fun onProgressChanged(view: WebView?, newProgress: Int) =
    inner.onProgressChanged(view, newProgress)

  override fun onReceivedTitle(view: WebView?, title: String?) = inner.onReceivedTitle(view, title)

  override fun onReceivedIcon(view: WebView?, icon: Bitmap?) = inner.onReceivedIcon(view, icon)

  override fun onReceivedTouchIconUrl(view: WebView?, url: String?, precomposed: Boolean) =
    inner.onReceivedTouchIconUrl(view, url, precomposed)

  override fun onShowCustomView(view: View?, callback: CustomViewCallback?) =
    inner.onShowCustomView(view, callback)

  override fun onHideCustomView() = inner.onHideCustomView()

  override fun onCreateWindow(
    view: WebView?,
    isDialog: Boolean,
    isUserGesture: Boolean,
    resultMsg: Message?
  ): Boolean = inner.onCreateWindow(view, isDialog, isUserGesture, resultMsg)

  override fun onRequestFocus(view: WebView?) = inner.onRequestFocus(view)

  override fun onCloseWindow(window: WebView?) = inner.onCloseWindow(window)

  override fun onJsAlert(view: WebView?, url: String?, message: String?, result: JsResult?): Boolean =
    inner.onJsAlert(view, url, message, result)

  override fun onJsConfirm(view: WebView?, url: String?, message: String?, result: JsResult?): Boolean =
    inner.onJsConfirm(view, url, message, result)

  override fun onJsPrompt(
    view: WebView?,
    url: String?,
    message: String?,
    defaultValue: String?,
    result: JsPromptResult?
  ): Boolean = inner.onJsPrompt(view, url, message, defaultValue, result)

  override fun onJsBeforeUnload(
    view: WebView?,
    url: String?,
    message: String?,
    result: JsResult?
  ): Boolean = inner.onJsBeforeUnload(view, url, message, result)

  override fun onGeolocationPermissionsHidePrompt() = inner.onGeolocationPermissionsHidePrompt()

  override fun onPermissionRequestCanceled(request: PermissionRequest?) =
    inner.onPermissionRequestCanceled(request)

  override fun onConsoleMessage(consoleMessage: ConsoleMessage?): Boolean =
    inner.onConsoleMessage(consoleMessage)

  override fun getDefaultVideoPoster(): Bitmap? = inner.defaultVideoPoster

  override fun getVideoLoadingProgressView(): View? = inner.videoLoadingProgressView

  override fun getVisitedHistory(callback: ValueCallback<Array<String>>?) =
    inner.getVisitedHistory(callback)

  override fun onShowFileChooser(
    webView: WebView?,
    filePathCallback: ValueCallback<Array<Uri>>?,
    fileChooserParams: FileChooserParams?
  ): Boolean = inner.onShowFileChooser(webView, filePathCallback, fileChooserParams)
}
