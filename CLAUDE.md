# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this project does

**cam-mobile-pc** streams the Android rear camera to a Linux PC as a virtual V4L2 webcam over USB (ADB forward) or WiFi. Full pipeline:

```
Android CameraX (1280×720, YUV_420_888) → NV21 → JPEG (quality 75) → MJPEG over TCP :5000
  → ADB forward (USB) → FFmpeg subprocess → yuv420p pipe → Rust V4l2Writer → /dev/video10
                                                          → RGB24 640×360 preview → egui GPU texture
```

The Linux side is a **Rust/egui app** (`linux/` is the Cargo crate root). FFmpeg is a subprocess.
Target platform: **Ubuntu** (`setup_ubuntu.sh` builds upstream `v4l2loopback`).

## Commands

### Build — Linux (Rust)

```bash
~/.cargo/bin/cargo build --release --manifest-path linux/Cargo.toml
./linux/target/release/campc
```

### Build — Android

```bash
cd mobile_cam_app
./gradlew assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk

# or build + install in one step:
./gradlew installDebug
```

### One-time Ubuntu setup

```bash
bash linux/setup_ubuntu.sh   # installs deps, builds v4l2loopback, persists module
```

### Verification

```bash
lsmod | grep v4l2loopback
v4l2-ctl --list-devices
cat /sys/module/v4l2loopback/parameters/exclusive_caps   # must be 1
nc localhost 5000 | head -c 300    # inspect raw MJPEG while phone is streaming
fuser /dev/video10
```

## Architecture

### Android (`mobile_cam_app/app/src/main/java/com/mobilecamapp/`)

- **`MainActivity.kt`** — Single-activity UI. Requests CAMERA + POST_NOTIFICATIONS permissions; starts/stops `CameraStreamingService` via Intent with `ACTION_START`/`ACTION_STOP`.

- **`CameraStreamingService.kt`** — `ForegroundService` (`foregroundServiceType="camera"`). Implements `LifecycleOwner` itself (so CameraX stays bound when Activity is gone). Holds `PARTIAL_WAKE_LOCK`. Orchestrates `TcpServer` + `CameraStreamer`. Persistent notification with Stop button.

- **`CameraStreamer.kt`** — Binds CameraX `ImageAnalysis`. Uses `ResolutionSelector` with `ResolutionStrategy(Size(1280,720), FALLBACK_RULE_CLOSEST_LOWER)` to prefer 16:9 capture modes over 4:3 fallbacks. JPEG quality **75**. `STRATEGY_KEEP_ONLY_LATEST` prevents backpressure/OOM.

- **`TcpServer.kt`** — Coroutine-based TCP server on port 5000. One client at a time. MIME multipart MJPEG framing (`--frame\r\n...`). Sets `TCP_NODELAY=true` + `sendBufferSize=524288` after accept. Sets `outputStream=null` on send failure (disconnect detection).

### Linux (`linux/` — Cargo crate)

- **`src/main.rs`** — egui `CamPCApp`. Catppuccin Mocha theme. Top panel: status. Bottom panel: FPS slider (5-30), Rotation buttons (0/90/180/270°), Iniciar/Detener/Salir. Central panel: 640×360 preview canvas (GPU texture, bilinear). `on_exit()` kills FFmpeg by PID synchronously + removes ADB forward.

- **`src/engine.rs`** — Engine thread state machine:
  - States: `Idle → WaitingDevice → Connecting → Streaming`
  - Polls `adb device_connected()` every 2s
  - Runs `adb forward tcp:PORT tcp:PORT`
  - Spawns FFmpeg; checks health every 200ms; respawns on exit (500ms delay)
  - `EngineCmd`: `Start`, `Stop`, `UpdateConfig(Config)`

- **`src/ffmpeg.rs`** — FFmpeg supervisor + frame reader.
  - `build_vf_filter(cfg)`: `[transpose] → crop=iw:iw*9/16 → scale=1280:720:in_range=full:out_range=limited → setsar=1`
  - `spawn_ffmpeg()`: runs FFmpeg with `-fflags nobuffer -flags low_delay -probesize 32 -analyzeduration 0 -f mpjpeg -i tcp://localhost:PORT -vf ... -f rawvideo -pix_fmt yuv420p -r FPS pipe:1`
  - Frame-reader thread: reads yuv420p frames → `V4l2Writer::write_frame()` → `yuv420p_to_preview_rgb()` → `preview_tx`
  - `PREVIEW_W=640`, `PREVIEW_H=360`, `OUTPUT_W=1280`, `OUTPUT_H=720`

- **`src/v4l2.rs`** — `V4l2Writer`: opens device, tries `VIDIOC_S_FMT` with `VIDEO_OUTPUT=2` first and falls back to `VIDEO_CAPTURE=1`, then writes frames via `write_all()`.

- **`src/adb.rs`** — `device_connected()`, `forward(port)`, `remove_forward(port)`.

- **`src/config.rs`** — `Config { fps, rotation, v4l2_device, adb_port, preview_fps, connection_mode, wifi_ip, zoom }`. Loads/saves TOML at `~/.config/campc/config.toml`. Defaults include `connection_mode=Wifi`, `zoom=1.0`.

## Threading model (Rust)

```
main thread        egui event loop → repaints at 30fps, drains preview_rx channel
engine thread      ADB poll + FFmpeg spawn/health/respawn loop (200ms tick)
frame-reader       spawned per FFmpeg instance; reads stdout pipe → V4L2 write + preview_tx send
```

## Key implementation notes

### V4L2 with exclusive_caps=1
`V4l2Writer` tries `VIDIOC_S_FMT` with `V4L2_BUF_TYPE_VIDEO_OUTPUT=2` first (newer v4l2loopback), then falls back to `V4L2_BUF_TYPE_VIDEO_CAPTURE=1` for older module/kernel combinations.

### CameraX resolution — why FALLBACK_RULE_CLOSEST_LOWER
`FALLBACK_RULE_CLOSEST_LOWER` keeps CameraX biased toward 16:9 modes around the requested 1280×720 instead of jumping to a higher 4:3 candidate.

### FFmpeg low-latency flags
`-probesize 32 -analyzeduration 0` eliminate the ~1-2s startup buffer. `-fflags nobuffer -flags low_delay` reduce per-frame buffering. Without these, FFmpeg buffers ~5MB before outputting the first frame.

### ADB port in engine.rs
`UpdateConfig` must save `old_port = config.adb_port` **before** overwriting `config = new_cfg`, then call `adb::remove_forward(old_port)`. The current code does this correctly.

### exclusive_caps=1 requirement
Without `exclusive_caps=1`, Zoom, Google Meet, and Teams won't recognize the device as a real capture camera (they filter by capability flags).

### After kernel upgrade
Re-run `setup_ubuntu.sh` — it rebuilds and reinstalls `v4l2loopback` for the new kernel.

## What does NOT exist (legacy, removed, or never built)
- `linux/campc.py` — Python/Tkinter prototype. No longer the primary app.
- `linux/launch.sh` / `linux/setup.sh` — Legacy scripts for Fedora/ffmpeg workflow.
- Zoom, resolution, or quality controls in the GUI — removed; output is fixed 1280×720, JPEG quality is fixed on Android.
