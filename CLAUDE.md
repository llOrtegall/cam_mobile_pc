# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this project does

**cam-mobile-pc** streams the Android rear camera to a Linux PC as a virtual V4L2 webcam over USB (via ADB forward). The full pipeline:

```
Android CameraX (YUV_420_888) → NV21 → JPEG (quality 75) → MJPEG over TCP :5000
  → ADB forward (USB) → ffmpeg → /dev/video10 (v4l2loopback "AndroidCam")
```

## Commands

### Android app

```bash
# Build debug APK
cd android
./gradlew assembleDebug

# Install on connected device
adb install -r app/build/outputs/apk/debug/app-debug.apk

# Build and install in one step
./gradlew installDebug
```

### Linux PC

```bash
# One-time setup (installs adb, ffmpeg, akmod-v4l2loopback; creates /dev/video10)
cd linux
bash setup.sh

# Daily use: forward ADB port and start ffmpeg loop
cd linux
bash start.sh
```

### Verification

```bash
lsmod | grep v4l2loopback          # confirm module is loaded
v4l2-ctl --list-devices             # confirm /dev/video10 "AndroidCam" exists
nc localhost 5000 | head -c 300    # inspect raw MJPEG stream from phone
fuser /dev/video10                  # check which process owns the device
```

## Architecture

The project has two independent components:

### Android (`android/app/src/main/java/com/campc/`)

- **`MainActivity.kt`** — Single-activity UI. Requests CAMERA and POST_NOTIFICATIONS permissions, then starts/stops `CameraStreamingService` via explicit Intent with `ACTION_START`/`ACTION_STOP`.

- **`CameraStreamingService.kt`** — `ForegroundService` with `foregroundServiceType="camera"` (required by Android API 34+). Orchestrates `TcpServer` + `CameraStreamer`. Manages lifecycle with a `CoroutineScope(Dispatchers.Main + SupervisorJob())`. Shows a persistent notification with a Stop action button.

- **`CameraStreamer.kt`** — Binds CameraX `ImageAnalysis` to `ProcessLifecycleOwner` (allows the service to own the camera independently of the Activity). Receives `YUV_420_888` frames, converts to NV21 manually (handles both `pixelStride==2` fast path and `pixelStride==1` slow path), compresses to JPEG via `YuvImage.compressToJpeg()`, and calls `TcpServer.sendFrame()`. Uses `STRATEGY_KEEP_ONLY_LATEST` to prevent frame backpressure/OOM.

- **`TcpServer.kt`** — Coroutine-based TCP server on port 5000. Accepts one client at a time. Wraps each JPEG in MIME multipart format (`--frame\r\nContent-Type: image/jpeg\r\nContent-Length: N\r\n\r\n<data>\r\n`). Sets `outputStream = null` on send failure so the service loop detects disconnection.

### Linux (`linux/`)

- **`setup.sh`** — Run once per machine/kernel upgrade. Installs `android-tools`, `ffmpeg`, `akmod-v4l2loopback` (via RPM Fusion, with source build as fallback). Loads the module with `exclusive_caps=1` and persists it via `/etc/modules-load.d/` and `/etc/modprobe.d/`.

- **`start.sh`** — Run daily. Waits for ADB device, establishes `adb forward tcp:5000 tcp:5000`, then runs ffmpeg in a loop. ffmpeg reads `-f mpjpeg` from `tcp://localhost:5000`, scales/converts to `yuyv422`, and writes to `/dev/video10`. Auto-restarts on disconnection.

## Key implementation notes

- `exclusive_caps=1` on v4l2loopback is critical — without it, Zoom/Meet/Teams won't recognize the device as a real capture camera.
- `CameraStreamingService` uses `ProcessLifecycleOwner.get()` (not the Activity's lifecycle) so the camera stays open when the user navigates away from the app.
- `TcpServer` accepts only one concurrent client; if a second PC connects, the first is closed first.
- The `start.sh` loop handles reconnection entirely on the Linux side — no reconnect logic is needed in the Android app.
- After a kernel upgrade, `setup.sh` must be re-run to rebuild the `akmod-v4l2loopback` module.
