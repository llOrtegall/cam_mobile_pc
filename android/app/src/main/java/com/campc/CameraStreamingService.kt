package com.campc

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.IBinder
import android.util.Log
import androidx.lifecycle.ProcessLifecycleOwner
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel

class CameraStreamingService : Service() {
    companion object {
        private const val TAG = "CameraStreamingService"
        private const val NOTIFICATION_ID = 1
        private const val CHANNEL_ID = "campc_channel"

        const val ACTION_START = "com.campc.ACTION_START"
        const val ACTION_STOP = "com.campc.ACTION_STOP"
    }

    private val serviceScope = CoroutineScope(Dispatchers.Main + SupervisorJob())
    private var tcpServer: TcpServer? = null
    private var cameraStreamer: CameraStreamer? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        createNotificationChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_START -> startStreaming()
            ACTION_STOP -> stopSelfAndCleanup()
        }
        return START_NOT_STICKY
    }

    private fun startStreaming() {
        startForeground(NOTIFICATION_ID, buildNotification("Waiting for connection…"))

        val server = TcpServer(
            port = 5000,
            onClientConnected = {
                Log.i(TAG, "PC connected")
                updateNotification("Streaming to PC on :5000")
            },
            onClientDisconnected = {
                Log.i(TAG, "PC disconnected")
                updateNotification("Waiting for connection…")
            },
        )
        tcpServer = server
        server.start(serviceScope)

        val streamer = CameraStreamer(server)
        cameraStreamer = streamer
        streamer.start(ProcessLifecycleOwner.get(), applicationContext)

        Log.i(TAG, "Streaming service started")
    }

    private fun stopSelfAndCleanup() {
        cleanup()
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    private fun cleanup() {
        cameraStreamer?.stop()
        tcpServer?.stop()
        cameraStreamer = null
        tcpServer = null
    }

    override fun onDestroy() {
        super.onDestroy()
        cleanup()
        serviceScope.cancel()
    }

    private fun buildNotification(contentText: String): Notification {
        val stopIntent = Intent(this, CameraStreamingService::class.java).apply {
            action = ACTION_STOP
        }
        val stopPendingIntent = PendingIntent.getService(
            this, 0, stopIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )

        return androidx.core.app.NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle("CamPC Streaming")
            .setContentText(contentText)
            .setSmallIcon(android.R.drawable.ic_menu_camera)
            .setOngoing(true)
            .addAction(android.R.drawable.ic_media_pause, "Stop", stopPendingIntent)
            .build()
    }

    private fun updateNotification(contentText: String) {
        val notification = buildNotification(contentText)
        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        manager.notify(NOTIFICATION_ID, notification)
    }

    private fun createNotificationChannel() {
        val channel = NotificationChannel(
            CHANNEL_ID,
            "Camera Streaming",
            NotificationManager.IMPORTANCE_LOW
        ).apply {
            description = "Shows while the camera is streaming to PC"
        }
        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        manager.createNotificationChannel(channel)
    }
}
