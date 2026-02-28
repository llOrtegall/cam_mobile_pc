# cam-mobile-pc

Use your Android rear camera as a virtual webcam on Linux via USB.
Works with Zoom, Google Meet, Teams, Discord, OBS, and any V4L2-compatible app.

---

## How it works

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  ANDROID                                                                     │
│                                                                              │
│  CameraX (rear camera, 1280×720 @ up to 30 fps)                             │
│       │ YUV_420_888 frames                                                   │
│       ▼                                                                      │
│  CameraStreamer.kt                                                           │
│       │ converts YUV → NV21 → JPEG (quality 75)                             │
│       ▼                                                                      │
│  TcpServer.kt                                                                │
│       │ wraps each JPEG in a MIME multipart frame (MJPEG)                   │
│       │ listens on TCP :5000                                                 │
└───────┼──────────────────────────────────────────────────────────────────────┘
        │  USB cable
        │  adb forward tcp:5000 tcp:5000  (handled automatically)
        ▼
┌──────────────────────────────────────────────────────────────────────────────┐
│  LINUX PC (Ubuntu)                                                           │
│                                                                              │
│  campc.py (Python/Tkinter GUI)                                               │
│       │ reads MJPEG stream via OpenCV (CAP_FFMPEG)                          │
│       │ applies zoom, rotation, FPS limiting                                 │
│       │ writes frames to /dev/video10 via pyfakewebcam                      │
│       │ shows live preview in the GUI window                                 │
│       ▼                                                                      │
│  /dev/video10  (v4l2loopback, label: "AndroidCam", exclusive_caps=1)        │
│       ▼                                                                      │
│  Zoom / Meet / Teams / Discord / OBS → see "AndroidCam" as a webcam         │
└──────────────────────────────────────────────────────────────────────────────┘
```

---

## Requirements

- **Android:** 8.0+ (API 26), physical rear camera
- **Linux:** Ubuntu (uses `v4l2loopback-dkms` via apt)
- **USB cable** with data (not charge-only)
- **USB debugging** enabled on the phone (Developer Options)

---

## Setup (one time)

### 1. Linux — install dependencies and create the virtual device

```bash
cd linux
bash setup_ubuntu.sh
```

This installs `android-tools-adb`, `v4l2loopback-dkms`, `v4l-utils`, and the Python packages (`opencv-python`, `pyfakewebcam`, `Pillow`). It also loads the v4l2loopback kernel module and persists it across reboots.

> After a kernel upgrade, re-run `setup_ubuntu.sh` so DKMS rebuilds the module for the new kernel.

### 2. Android — build and install the app

```bash
cd mobile_cam_app
./gradlew installDebug
```

Or open `mobile_cam_app/` in Android Studio and run directly on your device.

---

## Daily use

**Correct order:**

1. Connect the phone via USB
2. On the phone: open **CamPC** → tap **Start Streaming**
3. On the PC:
   ```bash
   cd linux
   bash launch.sh
   ```
4. The GUI window opens. Click **▶ Iniciar** to start receiving frames.
5. In Zoom / Meet / Teams → Settings → Video → Camera → select **AndroidCam**

> Open the video call app **after** campc.py is connected and showing the preview.
> If you already had it open, restart it so it detects the device.

---

## Android app

The app has a single screen with a **Start Streaming / Stop Streaming** toggle button and a status indicator.

- Tapping **Start Streaming** requests Camera and Notification permissions (if not already granted), then starts a foreground service that runs independently of the UI.
- The status dot turns green and shows `Streaming · TCP :5000` while active.
- The stream continues **even with the screen locked** — the service holds a `PARTIAL_WAKE_LOCK` and uses its own `LifecycleOwner` (independent of the Activity) so CameraX stays bound regardless of screen state.
- A persistent notification appears while streaming, with a **Stop** button to end the service from the notification shade.

---

## Linux GUI (campc.py)

The GUI window provides a live 640×360 preview and the following controls:

| Control | Variable | Range | Effect |
|---|---|---|---|
| **Zoom** slider | `zoom_var` | 1.0 – 4.0× | Centre-crops the frame and scales back to original size (no distortion) |
| **FPS** slider | `fps_var` | 5 – 30 fps | Limits the output frame rate |
| **Rotation** radio | `rotation_var` | 0 / 90 / 180 / 270° | Rotates the output frame |
| **Output** radio | `resolution_var` | 720p / 1080p / 480p | Sets the resolution written to `/dev/video10` |

The status bar shows **● Conectado** (green) when receiving frames, and the ADB indicator shows whether `adb forward` succeeded. On disconnect, the app retries automatically every 2 seconds.

> If a video call app (Zoom, Meet) is already connected to `/dev/video10` when you change resolution, the device format is locked by that reader and the resolution change will take effect only after closing the call.

---

## Project structure

```
cam-mobile-pc/
├── mobile_cam_app/
│   └── app/src/main/
│       ├── java/com/mobilecamapp/
│       │   ├── MainActivity.kt            # UI + permissions + service control
│       │   ├── CameraStreamingService.kt  # ForegroundService (type: camera)
│       │   │                              #   owns LifecycleOwner + WakeLock
│       │   ├── CameraStreamer.kt          # CameraX + YUV→NV21→JPEG conversion
│       │   └── TcpServer.kt              # TCP server + MJPEG framing
│       ├── res/layout/activity_main.xml
│       └── AndroidManifest.xml
├── linux/
│   ├── campc.py          # Python/Tkinter GUI app (main Linux component)
│   ├── launch.sh         # Daily launcher: reloads v4l2loopback + starts campc.py
│   └── setup_ubuntu.sh   # One-time setup (apt + pip3 + module persistence)
└── README.md
```

---

## How the pipeline works in detail

### Color conversion on Android

CameraX delivers frames as `YUV_420_888`. To compress with Android's `YuvImage` the format must be `NV21`:

1. **Y plane** is copied directly (full luminance).
2. **UV plane** — two cases:
   - `vPlane.pixelStride == 2` → hardware already delivered interleaved VU (semi-planar), copied directly (**fast path**).
   - `pixelStride == 1` → V and U bytes are interleaved manually (**slow path**).
3. `YuvImage.compressToJpeg()` compresses at quality 75 (~3–8 Mbps depending on content).

### MJPEG over TCP

The protocol is `multipart/x-mixed-replace`, the same used by IP cameras:

```
--frame\r\n
Content-Type: image/jpeg\r\n
Content-Length: <bytes>\r\n
\r\n
<jpeg data>
\r\n
--frame\r\n
...
```

OpenCV's `CAP_FFMPEG` backend reads this natively via `tcp://localhost:5000`.

### Virtual V4L2 device

- `v4l2loopback` creates `/dev/video10` with `exclusive_caps=1`, which makes the device announce itself as a capture camera (not output-only) — required for Zoom, Meet, and Teams to recognize it.
- `pyfakewebcam` converts RGB frames to YUYV and writes them to the device via `VIDIOC_S_FMT` + `os.write`.

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| Zoom/Meet shows no camera | Device not detected | Open the call **after** campc.py is connected; or restart the call app |
| `FakeWebcam error: device does not exist` | v4l2loopback not loaded | Run `bash linux/launch.sh` (it loads the module automatically) |
| `[Errno 16] Device or resource busy` | Another process holds `/dev/video10` | `launch.sh` reloads the module on every run, clearing stale handles |
| Stream stops when screen locks | (fixed) WakeLock + custom LifecycleOwner | Already handled in current app version |
| Resolution change has no effect | Video call app holds the device format lock | Close the call first, change resolution in campc.py, then rejoin |
| High latency | USB cable quality or background load | Try a different cable; close other apps |
| ADB forward fails | USB debugging not enabled or cable issue | Check Developer Options; unplug and replug |
| Module missing after kernel upgrade | DKMS needs rebuild | Re-run `bash linux/setup_ubuntu.sh` |

---

## Quick verification

```bash
# Confirm the module is loaded and device exists
lsmod | grep v4l2loopback
v4l2-ctl --list-devices

# Inspect the raw MJPEG stream from the phone
nc localhost 5000 | head -c 300

# Check which process owns the device
fuser /dev/video10
```

---

## Design decisions

| Decision | Choice | Why |
|---|---|---|
| Video protocol | MJPEG (MIME multipart) | Each frame is an independent JPEG; OpenCV reads it natively; resilient to packet loss |
| Transport | ADB forward over USB | No network config needed; low and stable latency; works in any environment |
| Virtual device | v4l2loopback with `exclusive_caps=1` | Appears as a real webcam to Zoom, Meet, Teams (they filter out devices without this flag) |
| Android encoding | CameraX + YuvImage | Simpler than MediaCodec; `STRATEGY_KEEP_ONLY_LATEST` prevents frame backlog and OOM |
| Linux GUI | Python/Tkinter + OpenCV + pyfakewebcam | No external process (no ffmpeg); native preview; live controls without restart |
| Resolution / FPS | 1280×720 @ up to 30 fps, JPEG quality 75 | ~3–8 Mbps; comfortably fits USB 2.0; sufficient for video calls |
