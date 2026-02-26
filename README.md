# cam-mobile-pc

Use your Android phone's rear camera as a virtual webcam on Linux (Fedora) via USB cable.
Works with Zoom, Google Meet, Teams, Discord, and any other V4L2-compatible app.

## Architecture

```
Android (CamPC app)                          Linux PC
┌────────────────────────────┐               ┌──────────────────────────────────────┐
│ CameraX ImageAnalysis       │               │ adb forward tcp:5000 tcp:5000        │
│ YUV → JPEG (quality 75)    │  USB cable    │                                      │
│ MIME multipart TCP :5000   │ ─────────────▶│ ffmpeg -f mpjpeg → /dev/video10      │
│ ForegroundService          │               │ (v4l2loopback virtual camera)        │
└────────────────────────────┘               │                                      │
                                              │ Zoom/Meet/Teams see "AndroidCam"     │
                                              └──────────────────────────────────────┘
```

## Quick Start

### 1. Linux: one-time setup

```bash
cd linux
bash setup.sh
```

This installs `adb`, `ffmpeg`, `v4l2loopback`, and persists the virtual camera device
(`/dev/video10`, label **AndroidCam**) across reboots.

### 2. Android: build and install the app

```bash
cd android
./gradlew assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

Or open the `android/` directory in Android Studio and run directly.

**Requirements:** Android 8.0+ (API 26), physical device with rear camera.

### 3. Stream

**On the phone:** open the **CamPC** app → tap **Start Streaming**.

**On the PC:**

```bash
cd linux
./start.sh
```

The script waits for the phone, sets up ADB port forwarding, and starts ffmpeg.
It auto-restarts on disconnect.

### 4. Use the virtual camera

Open Zoom / Meet / Teams → Settings → Video → Camera → select **AndroidCam**.

## File Structure

```
cam-mobile-pc/
├── android/
│   ├── app/src/main/
│   │   ├── java/com/campc/
│   │   │   ├── MainActivity.kt            # UI + permissions + service control
│   │   │   ├── CameraStreamingService.kt  # ForegroundService (camera type)
│   │   │   ├── CameraStreamer.kt          # CameraX binding + YUV→JPEG
│   │   │   └── TcpServer.kt              # MIME multipart framing over TCP
│   │   ├── res/layout/activity_main.xml
│   │   └── AndroidManifest.xml
│   ├── app/build.gradle.kts
│   └── settings.gradle.kts
├── linux/
│   ├── setup.sh    # Install deps, load v4l2loopback, persist at boot
│   └── start.sh    # ADB forward + ffmpeg loop (auto-restart)
└── README.md
```

## Troubleshooting

| Symptom | Fix |
|---|---|
| Camera not visible in Zoom/Meet | Confirm `exclusive_caps=1` in `modprobe.d/v4l2loopback.conf`; reload module |
| Green frame / color artifacts | Change `yuyv422` → `rgb24` in `start.sh` |
| High latency | Add `-probesize 32 -analyzeduration 0` to ffmpeg (already included) |
| "Device busy" on `/dev/video10` | Kill any other process using it: `fuser /dev/video10` |
| Android camera silently fails | Check `foregroundServiceType="camera"` is in `AndroidManifest.xml` |
| ADB forward fails after USB reconnect | `start.sh` retries automatically; check USB cable / developer options |

## Verification

```bash
# Check module loaded
lsmod | grep v4l2loopback
v4l2-ctl --device=/dev/video10 --info

# Test with synthetic source (no phone needed)
ffmpeg -f lavfi -i testsrc=size=1280x720:rate=30 -pix_fmt yuyv422 -f v4l2 /dev/video10 &
ffplay /dev/video10

# Check ADB forwarding
adb forward --list          # should show: tcp:5000 tcp:5000

# Peek at raw MJPEG stream header
nc localhost 5000 | head -c 200
```

## Key Design Decisions

| Decision | Choice | Reason |
|---|---|---|
| Video format | MJPEG (MIME multipart) | Each frame independent; ffmpeg `mpjpeg` demuxer built-in |
| Transport | ADB forward over USB | No WiFi needed; reliable, low latency |
| Virtual camera | v4l2loopback `exclusive_caps=1` | Appears as real camera to all V4L2 apps |
| Android encoding | CameraX + YuvImage | Simpler than MediaCodec; `STRATEGY_KEEP_ONLY_LATEST` prevents OOM |
| Resolution / FPS | 1280×720 @ 30fps, quality 75 | ~3–8 Mbps; fits comfortably on USB 2.0 |

## Phase 2: H.264 (future)

Replace `CameraStreamer.kt` encoding with `MediaCodec` H.264 + MPEG-TS packetizer.
On the Linux side change `-f mpjpeg` → `-f mpegts`. ~10× bandwidth reduction.
