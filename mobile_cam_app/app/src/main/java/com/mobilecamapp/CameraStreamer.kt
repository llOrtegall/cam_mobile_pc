package com.mobilecamapp

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

        val yRowStride = yPlane.rowStride
        val uvRowStride = uPlane.rowStride
        val uvPixelStride = uPlane.pixelStride

        val ySize = width * height
        val chromaSize = ySize / 2
        val nv21 = ByteArray(ySize + chromaSize)

        // Copy Y plane respecting row stride
        val yRow = ByteArray(yRowStride)
        var outputPos = 0
        yBuffer.rewind()
        for (row in 0 until height) {
            val toCopy = minOf(yRowStride, yBuffer.remaining())
            yBuffer.get(yRow, 0, toCopy)
            System.arraycopy(yRow, 0, nv21, outputPos, width)
            outputPos += width
        }

        // Copy interleaved VU (NV21) from U/V planes taking pixelStride into account
        val uRow = ByteArray(uvRowStride)
        val vRow = ByteArray(uvRowStride)
        val halfHeight = height / 2
        var chromaPos = ySize

        for (row in 0 until halfHeight) {
            val uvRowStart = row * uvRowStride

            // read full row from each plane
            uBuffer.position(uvRowStart)
            val uAvailable = minOf(uvRowStride, uBuffer.remaining())
            uBuffer.get(uRow, 0, uAvailable)

            vBuffer.position(uvRowStart)
            val vAvailable = minOf(uvRowStride, vBuffer.remaining())
            vBuffer.get(vRow, 0, vAvailable)

            var col = 0
            var j = 0
            while (col < width) {
                val uIndex = j * uvPixelStride
                val vIndex = j * vPlane.pixelStride
                val uByte = if (uIndex < uAvailable) uRow[uIndex] else 0
                val vByte = if (vIndex < vAvailable) vRow[vIndex] else 0

                nv21[chromaPos++] = vByte
                nv21[chromaPos++] = uByte

                col += 2
                j++
            }
        }

        val yuvImage = YuvImage(nv21, ImageFormat.NV21, width, height, null)
        val out = ByteArrayOutputStream()
        yuvImage.compressToJpeg(Rect(0, 0, width, height), JPEG_QUALITY, out)
        return out.toByteArray()
    }
}
