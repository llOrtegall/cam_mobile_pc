package com.mobilecamapp

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.IBinder
import android.os.PowerManager
import android.util.Log
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.LifecycleRegistry
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetAddress

class CameraStreamingService : Service(), LifecycleOwner {

    private val lifecycleRegistry = LifecycleRegistry(this)
    override val lifecycle: Lifecycle get() = lifecycleRegistry
    companion object {
        private const val TAG = "CameraStreamingService"
        private const val NOTIFICATION_ID = 1
        private const val CHANNEL_ID = "mobilecamapp_channel"

        const val ACTION_START = "com.mobilecamapp.ACTION_START"
        const val ACTION_STOP = "com.mobilecamapp.ACTION_STOP"

        private const val BEACON_PORT = 5001
        private const val BEACON_MSG = "CAMPC_HELLO"
        private const val BEACON_INTERVAL_MS = 2000L
    }

    private val serviceScope = CoroutineScope(Dispatchers.Main + SupervisorJob())
    private var tcpServer: TcpServer? = null
    private var cameraStreamer: CameraStreamer? = null
    private var wakeLock: PowerManager.WakeLock? = null
    private var beaconJob: Job? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        lifecycleRegistry.handleLifecycleEvent(Lifecycle.Event.ON_CREATE)
        lifecycleRegistry.handleLifecycleEvent(Lifecycle.Event.ON_START)
        lifecycleRegistry.handleLifecycleEvent(Lifecycle.Event.ON_RESUME)
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
        val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
        wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "mobilecamapp:streaming").apply {
            acquire()
        }

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
        streamer.start(this, applicationContext)

        startBeacon(serviceScope)

        Log.i(TAG, "Streaming service started")
    }

    /**
     * Broadcasts a UDP beacon every 2 s so the Rust app can discover this
     * device on the local network without requiring ADB/USB.
     *
     * The Rust side listens on port 5001 and reads the source IP from the
     * received packet — no payload metadata needed beyond the sentinel string.
     */
    private fun startBeacon(scope: CoroutineScope) {
        beaconJob = scope.launch(Dispatchers.IO) {
            try {
                DatagramSocket().use { socket ->
                    socket.broadcast = true
                    val msg = BEACON_MSG.toByteArray(Charsets.US_ASCII)
                    val broadcast = InetAddress.getByName("255.255.255.255")
                    val packet = DatagramPacket(msg, msg.size, broadcast, BEACON_PORT)
                    Log.i(TAG, "UDP beacon started → 255.255.255.255:$BEACON_PORT")
                    while (isActive) {
                        try {
                            socket.send(packet)
                        } catch (e: Exception) {
                            Log.w(TAG, "Beacon send failed: ${e.message}")
                        }
                        delay(BEACON_INTERVAL_MS)
                    }
                }
            } catch (e: Exception) {
                Log.e(TAG, "Beacon error: ${e.message}")
            }
        }
    }

    private fun stopSelfAndCleanup() {
        cleanup()
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    private fun cleanup() {
        beaconJob?.cancel()
        beaconJob = null
        cameraStreamer?.stop()
        tcpServer?.stop()
        cameraStreamer = null
        tcpServer = null
        wakeLock?.let { if (it.isHeld) it.release() }
        wakeLock = null
    }

    override fun onDestroy() {
        super.onDestroy()
        cleanup()
        serviceScope.cancel()
        lifecycleRegistry.handleLifecycleEvent(Lifecycle.Event.ON_PAUSE)
        lifecycleRegistry.handleLifecycleEvent(Lifecycle.Event.ON_STOP)
        lifecycleRegistry.handleLifecycleEvent(Lifecycle.Event.ON_DESTROY)
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
