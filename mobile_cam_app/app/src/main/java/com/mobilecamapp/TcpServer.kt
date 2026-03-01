package com.mobilecamapp

import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import java.io.IOException
import java.io.OutputStream
import java.net.ServerSocket
import java.net.Socket

class TcpServer(
    private val port: Int = 5000,
    private val onClientConnected: () -> Unit = {},
    private val onClientDisconnected: () -> Unit = {},
) {
    companion object {
        private const val TAG = "TcpServer"

        // Pre-computed static parts of the MJPEG frame header to avoid
        // allocating a new String + ByteArray on every frame.
        private val FRAME_PREFIX =
            "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: "
                .toByteArray(Charsets.US_ASCII)
        private val HEADER_END = "\r\n\r\n".toByteArray(Charsets.US_ASCII)
        private val FRAME_SUFFIX = "\r\n".toByteArray(Charsets.US_ASCII)
    }

    // Guards both `outputStream` and `currentClient` so that sendFrame and
    // closeClient never race: either sendFrame writes a complete frame or
    // closeClient nulls the stream — never both at the same time.
    private val streamLock = Any()

    private var outputStream: OutputStream? = null
    private var currentClient: Socket? = null

    private var serverSocket: ServerSocket? = null
    private var acceptJob: Job? = null

    fun start(scope: CoroutineScope) {
        acceptJob = scope.launch(Dispatchers.IO) {
            try {
                serverSocket = ServerSocket(port)
                Log.i(TAG, "TCP server listening on port $port")

                while (isActive) {
                    val client = try {
                        serverSocket?.accept() ?: break
                    } catch (e: IOException) {
                        if (isActive) Log.w(TAG, "Accept failed: ${e.message}")
                        break
                    }

                    Log.i(TAG, "Client connected: ${client.inetAddress}")
                    client.tcpNoDelay = true
                    client.sendBufferSize = 65536
                    synchronized(streamLock) {
                        currentClient = client
                        outputStream = client.getOutputStream()
                    }
                    onClientConnected()

                    // Wait until client disconnects (sendFrame will null outputStream on error).
                    while (isActive && synchronized(streamLock) { outputStream } != null) {
                        delay(200)
                    }

                    closeClient()
                    onClientDisconnected()
                    Log.i(TAG, "Client disconnected, waiting for new connection")
                    delay(500)
                }
            } catch (e: Exception) {
                Log.e(TAG, "Server error: ${e.message}")
            } finally {
                closeAll()
            }
        }
    }

    fun sendFrame(jpegBytes: ByteArray) {
        synchronized(streamLock) {
            val stream = outputStream ?: return
            try {
                // Header uses pre-computed byte arrays — only Content-Length varies.
                stream.write(FRAME_PREFIX)
                stream.write(jpegBytes.size.toString().toByteArray(Charsets.US_ASCII))
                stream.write(HEADER_END)
                stream.write(jpegBytes)
                stream.write(FRAME_SUFFIX)
                stream.flush()
            } catch (e: IOException) {
                Log.w(TAG, "Send failed, client likely disconnected: ${e.message}")
                outputStream = null
            }
        }
    }

    fun stop() {
        acceptJob?.cancel()
        closeAll()
    }

    private fun closeClient() {
        val clientToClose = synchronized(streamLock) {
            outputStream = null
            val c = currentClient
            currentClient = null
            c
        }
        try { clientToClose?.close() } catch (_: IOException) {}
    }

    private fun closeAll() {
        closeClient()
        try { serverSocket?.close() } catch (_: IOException) {}
        serverSocket = null
    }
}
