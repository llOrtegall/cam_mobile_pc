# cam-mobile-pc

Use your Android rear camera as a virtual webcam on Linux via USB.
Works with Zoom, Google Meet, Teams, Discord, OBS, and any V4L2-compatible app.

---

## How it works

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  ANDROID  (mobile_cam_app/)                                                  │
│                                                                              │
│  CameraX  — rear camera, 1920×1080 @ 30 fps (ResolutionSelector)            │
│       │ YUV_420_888 frames                                                   │
│       ▼                                                                      │
│  CameraStreamer.kt                                                           │
│       │ converts YUV → NV21 → JPEG (quality 95)                             │
│       ▼                                                                      │
│  TcpServer.kt                                                                │
│       │ wraps each JPEG in MIME multipart (MJPEG)                           │
│       │ listens on TCP :5000  (TCP_NODELAY, 64 KB send buffer)              │
└───────┼──────────────────────────────────────────────────────────────────────┘
        │  USB cable
        │  adb forward tcp:5000 tcp:5000  (managed by the Rust app)
        ▼
┌──────────────────────────────────────────────────────────────────────────────┐
│  LINUX PC — Ubuntu  (linux/)                                                 │
│                                                                              │
│  campc  (Rust/egui GUI)                                                      │
│    Engine thread                                                             │
│       │ polls ADB device presence every 2 s                                 │
│       │ runs adb forward tcp:5000 tcp:5000                                  │
│       │ spawns FFmpeg subprocess:                                            │
│       │   ffmpeg -fflags nobuffer -flags low_delay                          │
│       │          -probesize 32 -analyzeduration 0                           │
│       │          -f mpjpeg -i tcp://localhost:5000                           │
│       │          -vf "crop,scale=1920:1080,setsar=1"                        │
│       │          -f rawvideo -pix_fmt yuv420p pipe:1                        │
│    Frame-reader thread (per FFmpeg spawn)                                    │
│       │ reads yuv420p frames from FFmpeg stdout                             │
│       │ writes each frame to /dev/video10 via VIDIOC_S_FMT + write()       │
│       │ downscales to 640×360 RGB24 → mpsc channel → GUI preview texture   │
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
- **Rust toolchain** (`rustup` + `cargo`)
- **FFmpeg** (`sudo apt install ffmpeg`)

---

## Setup (one time)

### 1. Linux — install dependencies and create the virtual device

```bash
bash linux/setup_ubuntu.sh
```

Installs `android-tools-adb`, `v4l2loopback-dkms`, `v4l-utils`, `ffmpeg`. Loads the v4l2loopback kernel module with `exclusive_caps=1` and persists it across reboots.

> After a kernel upgrade, re-run `setup_ubuntu.sh` so DKMS rebuilds the module for the new kernel.

### 2. Build the Linux GUI app

```bash
~/.cargo/bin/cargo build --release --manifest-path linux/Cargo.toml
```

Binary lands at `linux/target/release/campc`.

### 3. Android — build and install the app

```bash
cd mobile_cam_app
./gradlew installDebug
```

Or open `mobile_cam_app/` in Android Studio and run directly on the device.

---

## Daily use

1. Connect the phone via USB
2. On the phone: open **CamPC** → tap **Start Streaming**
3. On the PC:
   ```bash
   ./linux/target/release/campc
   ```
4. The GUI window opens. Click **▶ Iniciar** to start receiving frames.
5. In Zoom / Meet / Teams → Settings → Video → Camera → select **AndroidCam**

> Open the video call app **after** campc is connected and showing the preview.
> If it was already open, restart it so it re-scans the V4L2 devices.

---

## Android app

Single screen with **Start Streaming / Stop Streaming** toggle.

- Tapping **Start Streaming** requests Camera and Notification permissions, then starts a foreground service that runs independently of the UI.
- Stream continues **even with the screen locked** — the service holds a `PARTIAL_WAKE_LOCK` and uses its own `LifecycleOwner` so CameraX stays bound regardless of screen state.
- A persistent notification appears while streaming, with a **Stop** button.

---

## Linux GUI (campc)

Catppuccin Mocha themed egui window with a live **640×360 preview canvas** and controls:

| Control | Range | Effect |
|---|---|---|
| **FPS** slider | 5 – 30 fps | Target framerate passed to FFmpeg `-r` |
| **Rotation** buttons | 0 / 90 / 180 / 270° | Adds `transpose` filter to FFmpeg `-vf` |
| **▶ Iniciar** | — | Starts ADB polling and FFmpeg pipeline |
| **■ Detener** | — | Kills FFmpeg, removes ADB forward |
| **Salir** | — | Graceful exit (kills FFmpeg orphans, saves config) |

Status indicator in the header bar: `Detenido` / `Esperando dispositivo…` / `Conectando…` / `● Transmitiendo` / `Error: …`

Config is saved to `~/.config/campc/config.toml` (fps, rotation, v4l2_device, adb_port).

---

## Project structure

```
cam-mobile-pc/
├── mobile_cam_app/
│   └── app/src/main/
│       ├── java/com/mobilecamapp/
│       │   ├── MainActivity.kt            # UI + permissions + service control
│       │   ├── CameraStreamingService.kt  # ForegroundService (type: camera)
│       │   │                              #   LifecycleOwner + WakeLock + TcpServer/CameraStreamer
│       │   ├── CameraStreamer.kt          # CameraX + YUV→NV21→JPEG (quality 95)
│       │   │                              #   ResolutionSelector → 1920×1080, FALLBACK_RULE_CLOSEST_LOWER
│       │   └── TcpServer.kt              # TCP server :5000, MJPEG framing, TCP_NODELAY
│       ├── res/layout/activity_main.xml
│       └── AndroidManifest.xml
└── linux/                                 # Cargo crate root (binary: campc)
    ├── Cargo.toml
    └── src/
        ├── main.rs     # egui App (CamPCApp), preview canvas, theme, on_exit cleanup
        ├── engine.rs   # State machine thread: ADB poll → FFmpeg spawn/health/respawn
        ├── ffmpeg.rs   # build_vf_filter(), spawn_ffmpeg(), frame reader thread, YUV→RGB preview
        ├── v4l2.rs     # V4l2Writer: VIDIOC_S_FMT (CAPTURE buf_type) + write() per frame
        ├── adb.rs      # device_connected(), forward(), remove_forward()
        └── config.rs   # Config struct, TOML load/save → ~/.config/campc/config.toml
```

---

## Pipeline details

### Color conversion on Android

CameraX delivers `YUV_420_888`. To compress with `YuvImage` the format must be `NV21`:

1. **Y plane** — copied directly (full luminance), row-stride-aware.
2. **UV plane** — interleaved as VU (NV21 order), taking `uvPixelStride` into account.
3. `YuvImage.compressToJpeg()` at quality **95** (~10–20 Mbps depending on content).

### MJPEG over TCP

Protocol: `multipart/x-mixed-replace`, same as IP cameras:

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

FFmpeg reads this with `-f mpjpeg`.

### FFmpeg video filter

```
crop=iw:iw*9/16:0:(ih-iw*9/16)/2    → crop to 16:9 (phone may send wider frames)
scale=1920:1080:in_range=full:out_range=limited  → rescale + JPEG full-range → TV limited-range
setsar=1                              → enforce square pixels
```

Rotation prepends `transpose=1` (90° CW) / `hflip,vflip` (180°) / `transpose=2` (90° CCW).

### V4L2 output (Rust, bypassing FFmpeg muxer)

`v4l2loopback` with `exclusive_caps=1` only advertises `V4L2_CAP_VIDEO_CAPTURE`. Using `VIDIOC_S_FMT` with `V4L2_BUF_TYPE_VIDEO_OUTPUT` returns EINVAL. The Rust `V4l2Writer` uses **`V4L2_BUF_TYPE_VIDEO_CAPTURE=1`** for the ioctl, then writes raw yuv420p frames directly via `write()`. This works reliably on kernel 6.x where FFmpeg's built-in v4l2 muxer fails.

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| Zoom/Meet shows no camera | Call opened before campc was streaming | Restart the call app after campc shows preview |
| `[v4l2] VIDIOC_S_FMT failed: Invalid argument` | Wrong buf_type or module not loaded | Confirm `exclusive_caps=1` module is loaded; `lsmod \| grep v4l2loopback` |
| `[v4l2] device unavailable — preview only` | `/dev/video10` doesn't exist | Run `setup_ubuntu.sh`; or `sudo modprobe v4l2loopback exclusive_caps=1` |
| FFmpeg exits immediately | Phone app not streaming yet | Start the Android app first; wait for "Waiting for connection…" notification |
| `speed=0.675x` in FFmpeg logs | Camera in 4:3 sensor mode (1920×1440) → ~20fps | `FALLBACK_RULE_CLOSEST_LOWER` in `CameraStreamer.kt` should force 16:9 mode |
| Preview looks pixelated | Window too small relative to preview texture | Resize the window larger; preview texture is 640×360 |
| High latency | TCP buffering or USB cable | `-probesize 32 -analyzeduration 0` flags already applied; try a better cable |
| ADB forward fails | USB debugging not enabled | Enable Developer Options → USB debugging on the phone |
| Module missing after kernel upgrade | DKMS needs rebuild | Re-run `bash linux/setup_ubuntu.sh` |

---

## Quick verification

```bash
lsmod | grep v4l2loopback            # confirm module is loaded
v4l2-ctl --list-devices              # confirm /dev/video10 "AndroidCam" exists
cat /sys/module/v4l2loopback/parameters/exclusive_caps  # should print 1
nc localhost 5000 | head -c 300      # inspect raw MJPEG stream from phone (while app streams)
fuser /dev/video10                   # check which process owns the device
```

---

## Design decisions

| Decision | Choice | Rationale |
|---|---|---|
| Video protocol | MJPEG (MIME multipart) | Each frame is independent; FFmpeg reads natively; resilient to packet loss |
| Transport | ADB forward over USB | No network config; low stable latency; works anywhere |
| Virtual device | v4l2loopback `exclusive_caps=1` | Zoom/Meet/Teams require this flag to recognise the device as a capture camera |
| Android encoding | CameraX + YuvImage JPEG 95 | Simple pipeline; `STRATEGY_KEEP_ONLY_LATEST` prevents backpressure/OOM |
| Linux app | Rust + egui + FFmpeg subprocess | Native performance; GPU preview; no Python runtime dependency |
| V4L2 write | Rust ioctls (CAPTURE buf_type) + write() | FFmpeg v4l2 muxer fails on kernel 6.x with `exclusive_caps=1`; CAPTURE type accepted |
| Preview | 640×360 yuv→rgb in frame-reader thread | Decoded in background; main thread only uploads texture to GPU |
| Resolution | 1920×1080 fixed output | Maximum quality; crop filter handles non-16:9 phone sensors |
