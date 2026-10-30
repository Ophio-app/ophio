// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.annotation.SuppressLint
import android.app.Activity
import android.content.Intent
import android.content.res.Configuration
import android.graphics.ImageFormat
import android.graphics.SurfaceTexture
import android.hardware.camera2.*
import android.media.ImageReader
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.view.Surface
import android.view.TextureView
import android.widget.LinearLayout
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import androidx.appcompat.widget.AppCompatButton
import androidx.core.view.WindowCompat
import com.google.zxing.PlanarYUVLuminanceSource
import java.util.concurrent.atomic.AtomicBoolean

/** Local camera frames are decoded in memory and never retained or uploaded. */
class ScannerActivity : AppCompatActivity(), TextureView.SurfaceTextureListener {
    private lateinit var preview: TextureView
    private var worker: HandlerThread? = null
    private var handler: Handler? = null
    @Volatile private var camera: CameraDevice? = null
    @Volatile private var capture: CameraCaptureSession? = null
    @Volatile private var reader: ImageReader? = null
    private var previewSurface: Surface? = null
    private val ended = AtomicBoolean(false)
    private val decoder = PairingCode.reader()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val spacing = (16 * resources.displayMetrics.density).toInt()
        val layout =
            LinearLayout(this).apply {
                orientation = LinearLayout.VERTICAL
                setPadding(spacing, spacing, spacing, spacing)
            }
        val dark =
            (resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK) ==
                Configuration.UI_MODE_NIGHT_YES
        layout.addView(
            TextView(this).apply {
                text = "Scan the code on your computer"
                textSize = 22f
            },
            LinearLayout.LayoutParams(-1, -2),
        )
        preview = TextureView(this).apply { surfaceTextureListener = this@ScannerActivity }
        layout.addView(preview, LinearLayout.LayoutParams(-1, 0, 1f))
        layout.addView(
            AppCompatButton(this).apply {
                text = "Cancel scan"
                isAllCaps = false
                minHeight = (56 * resources.displayMetrics.density).toInt()
                setOnClickListener { finish() }
            },
            LinearLayout.LayoutParams(-1, -2),
        )
        setContentView(layout)
        WindowCompat.getInsetsController(window, layout).apply {
            isAppearanceLightStatusBars = !dark
            isAppearanceLightNavigationBars = !dark
        }
        androidx.core.view.ViewCompat.setOnApplyWindowInsetsListener(layout) { v, i ->
            val b = i.getInsets(androidx.core.view.WindowInsetsCompat.Type.systemBars())
            v.setPadding(spacing + b.left, spacing + b.top, spacing + b.right, spacing + b.bottom)
            i
        }
    }

    override fun onResume() {
        super.onResume()
        ended.set(false)
        worker = HandlerThread("pairing-scanner").also { it.start() }
        handler = Handler(worker!!.looper)
        if (preview.isAvailable) open()
    }

    override fun onPause() {
        ended.set(true)
        capture?.close()
        capture = null
        camera?.close()
        camera = null
        reader?.close()
        reader = null
        previewSurface?.release()
        previewSurface = null
        worker?.quitSafely()
        worker = null
        handler = null
        super.onPause()
    }

    override fun onSurfaceTextureAvailable(surface: SurfaceTexture, width: Int, height: Int) {
        open()
    }

    override fun onSurfaceTextureSizeChanged(surface: SurfaceTexture, width: Int, height: Int) {}

    override fun onSurfaceTextureUpdated(surface: SurfaceTexture) {}

    override fun onSurfaceTextureDestroyed(surface: SurfaceTexture): Boolean = true

    private fun fail() {
        if (ended.compareAndSet(false, true))
            runOnUiThread {
                setResult(
                    Activity.RESULT_CANCELED,
                    Intent()
                        .putExtra(
                            "error",
                            "The camera could not scan. Close the scanner and try again.",
                        ),
                )
                finish()
            }
    }

    @SuppressLint("MissingPermission")
    private fun open() {
        if (ended.get() || camera != null || reader != null || handler == null) return
        try {
            val manager = getSystemService(CameraManager::class.java)
            val id =
                manager.cameraIdList.firstOrNull {
                    manager.getCameraCharacteristics(it).get(CameraCharacteristics.LENS_FACING) ==
                        CameraCharacteristics.LENS_FACING_BACK
                } ?: manager.cameraIdList.firstOrNull() ?: return fail()
            val sizes =
                manager
                    .getCameraCharacteristics(id)
                    .get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)
                    ?.getOutputSizes(ImageFormat.YUV_420_888) ?: return fail()
            val size =
                sizes
                    .filter { it.width <= 1280 && it.height <= 1280 }
                    .maxByOrNull { it.width * it.height } ?: sizes.minBy { it.width * it.height }
            val images =
                ImageReader.newInstance(size.width, size.height, ImageFormat.YUV_420_888, 2)
            reader = images
            images.setOnImageAvailableListener(
                { source ->
                    try {
                        source.acquireLatestImage()?.use { image ->
                            if (ended.get() || reader !== source) return@use
                            val plane = image.planes[0]
                            val buffer = plane.buffer
                            val y = ByteArray(image.width * image.height)
                            for (row in 0 until image.height) for (col in 0 until image.width) {
                                y[row * image.width + col] =
                                    buffer.get(row * plane.rowStride + col * plane.pixelStride)
                            }
                            val frame =
                                PlanarYUVLuminanceSource(
                                    y,
                                    image.width,
                                    image.height,
                                    0,
                                    0,
                                    image.width,
                                    image.height,
                                    false,
                                )
                            val result = PairingCode.read(decoder, frame)
                            if (
                                result != null &&
                                    result.text.length <= 16384 &&
                                    ended.compareAndSet(false, true)
                            )
                                runOnUiThread {
                                    setResult(
                                        Activity.RESULT_OK,
                                        Intent().putExtra("content", result.text),
                                    )
                                    finish()
                                }
                        }
                    } catch (_: Exception) {
                        if (!ended.get() && reader === source) fail()
                    }
                },
                handler,
            )
            preview.surfaceTexture?.setDefaultBufferSize(size.width, size.height)
            val surface = Surface(preview.surfaceTexture)
            previewSurface = surface
            manager.openCamera(
                id,
                object : CameraDevice.StateCallback() {
                    @Suppress("DEPRECATION")
                    override fun onOpened(device: CameraDevice) {
                        if (ended.get() || reader !== images) {
                            device.close()
                            return
                        }
                        camera = device
                        try {
                            device.createCaptureSession(
                                listOf(surface, images.surface),
                                object : CameraCaptureSession.StateCallback() {
                                    override fun onConfigured(session: CameraCaptureSession) {
                                        if (ended.get() || reader !== images) {
                                            session.close()
                                            return
                                        }
                                        capture = session
                                        try {
                                            val request =
                                                device
                                                    .createCaptureRequest(
                                                        CameraDevice.TEMPLATE_PREVIEW
                                                    )
                                                    .apply {
                                                        addTarget(surface)
                                                        addTarget(images.surface)
                                                        set(
                                                            CaptureRequest.CONTROL_AF_MODE,
                                                            CaptureRequest
                                                                .CONTROL_AF_MODE_CONTINUOUS_PICTURE,
                                                        )
                                                    }
                                                    .build()
                                            session.setRepeatingRequest(request, null, handler)
                                        } catch (_: Exception) {
                                            if (reader === images) fail()
                                        }
                                    }

                                    override fun onConfigureFailed(session: CameraCaptureSession) {
                                        if (reader === images) fail()
                                    }
                                },
                                handler,
                            )
                        } catch (_: Exception) {
                            if (reader === images) fail()
                        }
                    }

                    override fun onDisconnected(device: CameraDevice) {
                        device.close()
                        if (reader === images) fail()
                    }

                    override fun onError(device: CameraDevice, error: Int) {
                        device.close()
                        if (reader === images) fail()
                    }
                },
                handler,
            )
        } catch (_: Exception) {
            fail()
        }
    }
}
