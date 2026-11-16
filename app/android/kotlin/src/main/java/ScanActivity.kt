package io.github.irgendwer92.lebenszeichen.droid

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import android.view.Gravity
import android.view.ViewGroup
import android.widget.Button
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.TextView
import androidx.activity.ComponentActivity
import androidx.activity.OnBackPressedCallback
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.core.content.ContextCompat
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Scans a QR code (invitation, device code or setup code) with the back camera.
 *
 * Privacy: the frames are only examined in memory, by the Rust core, while
 * they are on screen. No frame, photo or video is stored or sent anywhere;
 * the only thing that leaves this activity is the text of the code found.
 *
 * Result: `RESULT_OK` with [EXTRA_CODE], or `RESULT_CANCELED`, with
 * [EXTRA_ERROR] if the camera cannot be used.
 */
class ScanActivity : ComponentActivity() {
    private var analysisExecutor: ExecutorService? = null
    private var cameraProvider: ProcessCameraProvider? = null
    private val finished = AtomicBoolean(false)

    private val permissionLauncher =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
            if (granted) {
                startCamera()
            } else {
                finishOnce { setResult(RESULT_CANCELED, Intent().putExtra(EXTRA_ERROR, getString(R.string.scan_error_no_camera))) }
            }
        }

    private lateinit var previewView: PreviewView

    /**
     * Shows the texts in the language chosen in the app. Runs on the main
     * thread; [CoreBridge.language] only reads a small local file.
     */
    override fun attachBaseContext(newBase: Context) {
        super.attachBaseContext(Texts.localized(newBase))
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val padding = (16 * resources.displayMetrics.density).toInt()

        previewView = PreviewView(this)
        val hint =
            TextView(this).apply {
                setText(R.string.scan_hint)
                textSize = 18f
                gravity = Gravity.CENTER
                setTextColor(0xFFFFFFFF.toInt())
                setBackgroundColor(0x99000000.toInt())
                setPadding(padding, padding, padding, padding)
            }
        val cancel =
            Button(this).apply {
                setText(R.string.scan_cancel)
                setOnClickListener { cancelScan() }
            }
        val overlay =
            LinearLayout(this).apply {
                orientation = LinearLayout.VERTICAL
                fitsSystemWindows = true
                setPadding(padding, padding, padding, padding)
                addView(hint, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
                // Pushes the button to the bottom.
                addView(android.view.View(this@ScanActivity), LinearLayout.LayoutParams(0, 0, 1f))
                addView(cancel, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
            }
        val root =
            FrameLayout(this).apply {
                addView(previewView, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
                addView(overlay, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
            }
        setContentView(root)

        onBackPressedDispatcher.addCallback(
            this,
            object : OnBackPressedCallback(true) {
                override fun handleOnBackPressed() {
                    cancelScan()
                }
            },
        )

        if (ContextCompat.checkSelfPermission(this, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) {
            startCamera()
        } else {
            permissionLauncher.launch(Manifest.permission.CAMERA)
        }
    }

    private fun startCamera() {
        val executor = Executors.newSingleThreadExecutor()
        analysisExecutor = executor
        val future = ProcessCameraProvider.getInstance(this)
        future.addListener(
            {
                if (isFinishing || isDestroyed) return@addListener
                val provider =
                    try {
                        future.get()
                    } catch (e: Exception) {
                        finishOnce {
                            setResult(RESULT_CANCELED, Intent().putExtra(EXTRA_ERROR, getString(R.string.scan_error_camera_start)))
                        }
                        return@addListener
                    }
                cameraProvider = provider
                val preview = Preview.Builder().build().also { it.setSurfaceProvider(previewView.surfaceProvider) }
                val analysis =
                    ImageAnalysis.Builder()
                        .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                        .setOutputImageFormat(ImageAnalysis.OUTPUT_IMAGE_FORMAT_YUV_420_888)
                        .build()
                        .also { it.setAnalyzer(executor) { image -> analyze(image) } }
                try {
                    provider.unbindAll()
                    provider.bindToLifecycle(this, CameraSelector.DEFAULT_BACK_CAMERA, preview, analysis)
                } catch (e: Exception) {
                    finishOnce {
                        setResult(RESULT_CANCELED, Intent().putExtra(EXTRA_ERROR, getString(R.string.scan_error_camera_start)))
                    }
                }
            },
            ContextCompat.getMainExecutor(this),
        )
    }

    /** Runs on the analysis thread. The frame is read in place and always closed. */
    private fun analyze(image: ImageProxy) {
        try {
            if (finished.get()) return
            val plane = image.planes[0]
            val code = CoreBridge.decodeQr(plane.buffer, image.width, image.height, plane.rowStride)
            if (code != null) {
                runOnUiThread { finishOnce { setResult(RESULT_OK, Intent().putExtra(EXTRA_CODE, code)) } }
            }
        } catch (e: Exception) {
            // A frame that cannot be read is skipped; the next one follows.
        } finally {
            image.close()
        }
    }

    private fun cancelScan() {
        finishOnce { setResult(RESULT_CANCELED) }
    }

    /** Sets the result and closes, only the first time. Call on the main thread. */
    private fun finishOnce(block: () -> Unit) {
        if (finished.compareAndSet(false, true)) {
            block()
            finish()
        }
    }

    override fun onDestroy() {
        cameraProvider?.unbindAll()
        analysisExecutor?.shutdown()
        super.onDestroy()
    }

    companion object {
        /** The text of the code found, in the result of the activity. */
        const val EXTRA_CODE = "code"

        /** Why the scan is impossible (in the chosen language, for the user), with `RESULT_CANCELED`. */
        const val EXTRA_ERROR = "error"

    }
}
