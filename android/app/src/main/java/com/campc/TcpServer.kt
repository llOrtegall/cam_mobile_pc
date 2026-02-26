package com.campc

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
        private const val BOUNDARY = "frame"
    }

    @Volatile
    private var outputStream: OutputStream? = null

    @Volatile
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
                    currentClient = client
                    outputStream = client.getOutputStream()
                    onClientConnected()

                    // Wait until client disconnects (sendFrame will set outputStream=null on error)
                    while (isActive && outputStream != null) {
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
        val stream = outputStream ?: return
        try {
            val header = "--$BOUNDARY\r\n" +
                "Content-Type: image/jpeg\r\n" +
                "Content-Length: ${jpegBytes.size}\r\n\r\n"
            stream.write(header.toByteArray(Charsets.US_ASCII))
            stream.write(jpegBytes)
            stream.write("\r\n".toByteArray(Charsets.US_ASCII))
            stream.flush()
        } catch (e: IOException) {
            Log.w(TAG, "Send failed, client likely disconnected: ${e.message}")
            outputStream = null
        }
    }

    fun stop() {
        acceptJob?.cancel()
        closeAll()
    }

    private fun closeClient() {
        outputStream = null
        try { currentClient?.close() } catch (_: IOException) {}
        currentClient = null
    }

    private fun closeAll() {
        closeClient()
        try { serverSocket?.close() } catch (_: IOException) {}
        serverSocket = null
    }
}
