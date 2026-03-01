package com.mobilecamapp

import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import java.io.BufferedOutputStream
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

        private const val CLIENT_BUFFER_BYTES = 524288
        private const val DISCONNECT_POLL_MS = 200L
        private const val RECONNECT_DELAY_MS = 500L
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
                    val client = acceptClient() ?: break
                    configureClient(client)
                    onClientConnected()

                    waitForClientDisconnect()

                    closeClient()
                    onClientDisconnected()
                    Log.i(TAG, "Client disconnected, waiting for new connection")
                    delay(RECONNECT_DELAY_MS)
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
                // Full frame write is atomic with respect to closeClient under streamLock.
                // Header bytes are precomputed; only Content-Length is generated per frame.
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

    private fun acceptClient(): Socket? {
        return try {
            serverSocket?.accept()
        } catch (e: IOException) {
            if (acceptJob?.isActive == true) {
                Log.w(TAG, "Accept failed: ${e.message}")
            }
            null
        }
    }

    private fun configureClient(client: Socket) {
        Log.i(TAG, "Client connected: ${client.inetAddress}")
        client.tcpNoDelay = true
        client.sendBufferSize = CLIENT_BUFFER_BYTES
        synchronized(streamLock) {
            currentClient = client
            outputStream = BufferedOutputStream(client.getOutputStream(), CLIENT_BUFFER_BYTES)
        }
    }

    private suspend fun waitForClientDisconnect() {
        while (acceptJob?.isActive == true && isClientActive()) {
            delay(DISCONNECT_POLL_MS)
        }
    }

    private fun isClientActive(): Boolean =
        synchronized(streamLock) { outputStream != null }

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
