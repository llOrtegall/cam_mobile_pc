package com.campc

import android.graphics.ImageFormat
import android.graphics.Rect
import android.graphics.YuvImage
import android.util.Log
import android.util.Size
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.lifecycle.LifecycleOwner
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

class CameraStreamer(
    private val tcpServer: TcpServer,
) {
    companion object {
        private const val TAG = "CameraStreamer"
        private const val JPEG_QUALITY = 75
        private val TARGET_RESOLUTION = Size(1280, 720)
    }

    private val analyzerExecutor: ExecutorService = Executors.newSingleThreadExecutor()
    private var cameraProvider: ProcessCameraProvider? = null

    fun start(lifecycleOwner: LifecycleOwner, context: android.content.Context) {
        val providerFuture = ProcessCameraProvider.getInstance(context)
        providerFuture.addListener({
            cameraProvider = providerFuture.get()
            bindCamera(lifecycleOwner)
        }, context.mainExecutor)
    }

    private fun bindCamera(lifecycleOwner: LifecycleOwner) {
        val provider = cameraProvider ?: return

        val imageAnalysis = ImageAnalysis.Builder()
            .setTargetResolution(TARGET_RESOLUTION)
            .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
            .setOutputImageFormat(ImageAnalysis.OUTPUT_IMAGE_FORMAT_YUV_420_888)
            .build()

        imageAnalysis.setAnalyzer(analyzerExecutor) { imageProxy ->
            try {
                val jpeg = imageProxy.toJpegBytes()
                tcpServer.sendFrame(jpeg)
            } finally {
                imageProxy.close()
            }
        }

        try {
            provider.unbindAll()
            provider.bindToLifecycle(lifecycleOwner, CameraSelector.DEFAULT_BACK_CAMERA, imageAnalysis)
            Log.i(TAG, "Camera bound successfully")
        } catch (e: Exception) {
            Log.e(TAG, "Camera binding failed: ${e.message}")
        }
    }

    fun stop() {
        cameraProvider?.unbindAll()
        analyzerExecutor.shutdown()
    }

    private fun ImageProxy.toJpegBytes(): ByteArray {
        val yPlane = planes[0]
        val uPlane = planes[1]
        val vPlane = planes[2]

        val yBuffer: ByteBuffer = yPlane.buffer
        val uBuffer: ByteBuffer = uPlane.buffer
        val vBuffer: ByteBuffer = vPlane.buffer

        val ySize = yBuffer.remaining()
        val uSize = uBuffer.remaining()
        val vSize = vBuffer.remaining()

        // Build NV21: Y plane followed by interleaved VU
        val nv21 = ByteArray(ySize + uSize + vSize)
        yBuffer.get(nv21, 0, ySize)

        // Fast path: semi-planar layout (pixelStride == 2 means VU already interleaved)
        if (vPlane.pixelStride == 2) {
            vBuffer.get(nv21, ySize, vSize)
        } else {
            // Slow path: manually interleave V and U bytes
            val vBytes = ByteArray(vSize)
            val uBytes = ByteArray(uSize)
            vBuffer.get(vBytes)
            uBuffer.get(uBytes)
            var idx = ySize
            for (i in 0 until minOf(vSize, uSize)) {
                nv21[idx++] = vBytes[i]
                nv21[idx++] = uBytes[i]
            }
        }

        val yuvImage = YuvImage(nv21, ImageFormat.NV21, width, height, null)
        val out = ByteArrayOutputStream()
        yuvImage.compressToJpeg(Rect(0, 0, width, height), JPEG_QUALITY, out)
        return out.toByteArray()
    }
}
