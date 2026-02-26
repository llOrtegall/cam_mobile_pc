package com.campc

import android.Manifest
import android.annotation.SuppressLint
import android.content.Intent
import android.content.res.ColorStateList
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import com.campc.databinding.ActivityMainBinding

class MainActivity : AppCompatActivity() {

    private lateinit var binding: ActivityMainBinding
    private var isStreaming = false

    @SuppressLint("SetTextI18n")
    private val permissionLauncher = registerForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions()
    ) { permissions ->
        val cameraGranted = permissions[Manifest.permission.CAMERA] == true
        if (cameraGranted) {
            startStreamingService()
        } else {
            binding.statusText.text = "Camera permission denied"
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        binding = ActivityMainBinding.inflate(layoutInflater)
        setContentView(binding.root)

        binding.toggleButton.setOnClickListener {
            if (isStreaming) stopStreamingService() else requestPermissionsAndStart()
        }

        updateUi(false)
    }

    private fun requestPermissionsAndStart() {
        val permissionsNeeded = buildList {
            if (!hasCameraPermission()) add(Manifest.permission.CAMERA)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                if (ContextCompat.checkSelfPermission(
                        this@MainActivity, Manifest.permission.POST_NOTIFICATIONS
                    ) != PackageManager.PERMISSION_GRANTED
                ) {
                    add(Manifest.permission.POST_NOTIFICATIONS)
                }
            }
        }

        if (permissionsNeeded.isEmpty()) {
            startStreamingService()
        } else {
            permissionLauncher.launch(permissionsNeeded.toTypedArray())
        }
    }

    private fun startStreamingService() {
        val intent = Intent(this, CameraStreamingService::class.java).apply {
            action = CameraStreamingService.ACTION_START
        }
        startForegroundService(intent)
        updateUi(true)
    }

    private fun stopStreamingService() {
        val intent = Intent(this, CameraStreamingService::class.java).apply {
            action = CameraStreamingService.ACTION_STOP
        }
        startService(intent)
        updateUi(false)
    }

    @SuppressLint("SetTextI18n")
    private fun updateUi(streaming: Boolean) {
        isStreaming = streaming
        binding.toggleButton.text = if (streaming) "Stop Streaming" else "Start Streaming"
        binding.statusText.text = if (streaming) "Streaming · TCP :5000" else "Ready"
        val dotColor = if (streaming)
            ContextCompat.getColor(this, R.color.status_streaming)
        else
            ContextCompat.getColor(this, R.color.status_idle)
        binding.statusDot.backgroundTintList = ColorStateList.valueOf(dotColor)
    }

    private fun hasCameraPermission() =
        ContextCompat.checkSelfPermission(this, Manifest.permission.CAMERA) ==
            PackageManager.PERMISSION_GRANTED

    override fun onDestroy() {
        super.onDestroy()
        // Service runs independently as a foreground service
    }
}
