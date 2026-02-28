# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this project does

**cam-mobile-pc** streams the Android rear camera to a Linux PC as a virtual V4L2 webcam over USB (via ADB forward). The full pipeline:

```
Android CameraX (YUV_420_888) → NV21 → JPEG (quality 75) → MJPEG over TCP :5000
  → ADB forward (USB) → campc.py (OpenCV + pyfakewebcam) → /dev/video10 (v4l2loopback "AndroidCam")
```

The Linux side is a **Python/Tkinter GUI app** (`campc.py`) — no ffmpeg required for normal use.
Target platform: **Ubuntu** (uses `v4l2loopback-dkms` via apt, not akmod/RPM Fusion).

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

### Linux PC (Ubuntu)

```bash
# One-time setup (installs adb, v4l2loopback-dkms, Python deps; creates /dev/video10)
bash linux/setup_ubuntu.sh

# Daily use: launch the GUI app (handles ADB forward internally)
python3 linux/campc.py
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

- **`setup_ubuntu.sh`** — Run once per machine (Ubuntu only). Installs `android-tools-adb`, `v4l2loopback-dkms`, `v4l-utils`, and Python packages (`opencv-python`, `pyfakewebcam`, `Pillow`). Loads the module with `exclusive_caps=1` and persists via `/etc/modules-load.d/` and `/etc/modprobe.d/`.

- **`campc.py`** — Main GUI app (Python/Tkinter). Threading model: Tkinter event loop on main thread + capture thread. The capture thread calls `adb forward`, opens `cv2.VideoCapture("tcp://localhost:5000", CAP_FFMPEG)`, and per frame: applies zoom (centre-crop + resize), rotation (`cv2.rotate`), resizes to output resolution, writes to `/dev/video10` via `pyfakewebcam.FakeWebcam`, and pushes a 640×360 thumbnail to the Tkinter canvas via `root.after()`. On disconnect, retries every 2 s automatically. Cleans up `adb forward --remove` on exit via `atexit`.

  Controls exposed in the UI:
  | Control | Variable | Range |
  |---|---|---|
  | Zoom slider | `zoom_var` DoubleVar | 1.0 – 4.0× |
  | FPS slider | `fps_var` IntVar | 5 – 30 |
  | Rotation radio | `rotation_var` IntVar | 0 / 90 / 180 / 270° |
  | Output resolution radio | `resolution_var` StringVar | 720p / 1080p / 480p |

- **`setup.sh`** / **`start.sh`** — Legacy Fedora/ffmpeg scripts (kept for reference, not the primary workflow).

## Key implementation notes

- `exclusive_caps=1` on v4l2loopback is critical — without it, Zoom/Meet/Teams won't recognize the device as a real capture camera.
- `CameraStreamingService` uses `ProcessLifecycleOwner.get()` (not the Activity's lifecycle) so the camera stays open when the user navigates away from the app.
- `TcpServer` accepts only one concurrent client; if a second PC connects, the first is closed first.
- `campc.py` handles reconnection automatically in the capture thread — no reconnect logic is needed in the Android app.
- `pyfakewebcam.FakeWebcam` is recreated whenever the output resolution changes (it must be constructed with fixed dimensions).
- After a kernel upgrade, `setup_ubuntu.sh` must be re-run so DKMS rebuilds the `v4l2loopback` module for the new kernel.
- All UI updates from the capture thread use `root.after(0, ...)` — never call Tkinter widgets directly from a background thread.
