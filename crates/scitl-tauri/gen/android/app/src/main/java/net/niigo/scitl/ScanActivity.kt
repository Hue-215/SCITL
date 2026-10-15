package net.niigo.scitl

import android.Manifest
import android.content.pm.PackageManager
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.core.content.ContextCompat
import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.ReaderException
import com.google.zxing.common.HybridBinarizer
import com.google.zxing.qrcode.QRCodeReader
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

/**
 * カメラの映像からQRを読む画面(QrScanner)。読めたら閉じ、「戻る」で取りやめる。カメラの許可は
 * ここで初めて求める。読んだ中身はそのままRust側へ渡し、確かめるのはRust側
 * (scitl_sync::parse_invitation)。
 *
 * 向きを変えても作り直さない(マニフェストのconfigChanges)。作り直すと、破棄した側の映像の処理が
 * 結果を置き、前に残った新しい側の読み取りが無視される。
 */
class ScanActivity : ComponentActivity() {
  private var id = 0L
  private var delivered = false
  private lateinit var preview: PreviewView
  private val analysis: ExecutorService = Executors.newSingleThreadExecutor()
  private val reader = QRCodeReader()
  private val hints = mapOf(DecodeHintType.POSSIBLE_FORMATS to listOf(BarcodeFormat.QR_CODE))

  private val askForCamera =
    registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
      if (granted) startCamera() else deliver(QrScanner.DENIED)
    }

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    id = intent.getLongExtra(QrScanner.EXTRA_ID, 0L)
    // 開くまでの間に取りやめられたか、次の読み取りが始まった。
    if (!QrScanner.attach(id, this)) {
      delivered = true
      finish()
      return
    }
    preview = PreviewView(this)
    setContentView(preview)
    // OSがプロセスごと作り直したときは、許可を求めている途中かもしれないので求め直さない。
    if (checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) {
      startCamera()
    } else if (savedInstanceState == null) {
      askForCamera.launch(Manifest.permission.CAMERA)
    }
  }

  override fun onDestroy() {
    // 「戻る」等で閉じた。
    if (isFinishing && !delivered) QrScanner.finish(id, QrScanner.CANCELLED)
    analysis.shutdown()
    super.onDestroy()
  }

  private fun startCamera() {
    val future = ProcessCameraProvider.getInstance(this)
    future.addListener({
      try {
        val provider = future.get()
        val camera = if (provider.hasCamera(CameraSelector.DEFAULT_BACK_CAMERA)) {
          CameraSelector.DEFAULT_BACK_CAMERA
        } else {
          CameraSelector.DEFAULT_FRONT_CAMERA
        }
        val shown = Preview.Builder().build().also { it.setSurfaceProvider(preview.surfaceProvider) }
        val frames = ImageAnalysis.Builder()
          .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
          .build()
          .also { it.setAnalyzer(analysis, ::decode) }
        provider.unbindAll()
        provider.bindToLifecycle(this, camera, shown, frames)
      } catch (e: Exception) {
        deliver(QrScanner.FAILED + e)
      }
    }, ContextCompat.getMainExecutor(this))
  }

  // 映像の1枚から読む。明るさ(YUVのY)だけを使う。
  private fun decode(image: ImageProxy) {
    image.use {
      val plane = it.planes[0]
      val buffer = plane.buffer
      val data = ByteArray(plane.rowStride * it.height)
      // 最後の行は、行の幅の分までしか無いことがある。
      buffer.get(data, 0, minOf(buffer.remaining(), data.size))
      val source = PlanarYUVLuminanceSource(
        data, plane.rowStride, it.height, 0, 0, it.width, it.height, false
      )
      try {
        val text = reader.decode(BinaryBitmap(HybridBinarizer(source)), hints).text
        runOnUiThread { deliver(QrScanner.SCANNED + text) }
      } catch (e: ReaderException) {
        // この1枚にはQRが無いか、読めなかった。次の1枚を待つ。
      } finally {
        reader.reset()
      }
    }
  }

  private fun deliver(result: String) {
    if (delivered || isDestroyed) return
    delivered = true
    QrScanner.finish(id, result)
    finish()
  }
}
