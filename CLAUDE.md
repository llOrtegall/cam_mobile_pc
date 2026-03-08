# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this project does

**cam-mobile-pc** streams the Android rear camera to a PC as a virtual webcam over USB (ADB forward) or WiFi. Two native host implementations exist:

| Crate | Platform | Virtual camera |
|---|---|---|
| `linux/` | Ubuntu (kernel 6.x) | `/dev/video10` via v4l2loopback |
| `windows/` | Windows 11 22H2+ | `IMFVirtualCamera` (MediaFoundation) |

### Full pipeline

```
Android CameraX (1280×720, YUV_420_888) → NV21 → JPEG (quality 85) → MJPEG over TCP :5000
  ├─ USB: ADB forward tcp:5000 → localhost:5000
  └─ WiFi: direct TCP to phone IP
        ↓
  FFmpeg subprocess
  (-fflags nobuffer -flags low_delay -probesize 32 -analyzeduration 0
   -f mpjpeg -i tcp://... -vf [rotate,crop,scale,setsar] -f rawvideo -pix_fmt yuv420p pipe:1)
        ↓
  Frame-reader thread
  ├─ Linux:   yuv420p → V4l2Writer (write_all to /dev/video10)
  └─ Windows: yuv420p → NV12 → VirtualCamWriter (IMFSample → MEMediaSample event)
        ↓
  yuv420p → RGB24 640×360 → preview_tx → egui GPU texture
```

FFmpeg is a subprocess on both platforms. The egui/eframe UI is identical.

---

## Commands


### Build — Linux (Rust)

```bash
~/.cargo/bin/cargo build --release --manifest-path linux/Cargo.toml
./linux/target/release/campc
```

### Build — Windows (Rust, run on Windows)

```powershell
cargo build --release --manifest-path windows/Cargo.toml
.\windows\target\release\campc.exe
```

Requirements: Rust MSVC toolchain (`x86_64-pc-windows-msvc`), Visual Studio Build Tools (Desktop C++), FFmpeg in PATH. ADB only needed for USB mode.

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

### Verification — Linux

```bash
lsmod | grep v4l2loopback
v4l2-ctl --list-devices
cat /sys/module/v4l2loopback/parameters/exclusive_caps   # must be 1
nc localhost 5000 | head -c 300    # inspect raw MJPEG while phone is streaming
fuser /dev/video10
```

### Verification — Windows

```powershell
ffmpeg -version                     # must resolve
adb version                         # only needed for USB mode
# After launching campc.exe and clicking Iniciar:
# Settings → Privacy → Camera → "AndroidCam" must appear
# Zoom/Teams → Settings → Video → select "AndroidCam"
```

---

## Architecture

### Android (`mobile_cam_app/app/src/main/java/com/mobilecamapp/`)

- **`MainActivity.kt`** — Single-activity UI. Requests CAMERA + POST_NOTIFICATIONS permissions; starts/stops `CameraStreamingService` via Intent with `ACTION_START`/`ACTION_STOP`.

- **`CameraStreamingService.kt`** — `ForegroundService` (`foregroundServiceType="camera"`). Implements `LifecycleOwner` itself (so CameraX stays bound when Activity is gone). Holds `PARTIAL_WAKE_LOCK`. Orchestrates `TcpServer` + `CameraStreamer`. Persistent notification with Stop button.

- **`CameraStreamer.kt`** — Binds CameraX `ImageAnalysis`. Uses `ResolutionSelector` with `ResolutionStrategy(Size(1280,720), FALLBACK_RULE_CLOSEST_LOWER)` to prefer 16:9 capture modes over 4:3 fallbacks. JPEG quality **85**. `STRATEGY_KEEP_ONLY_LATEST` prevents backpressure/OOM.

- **`TcpServer.kt`** — Coroutine-based TCP server on port 5000. One client at a time. MIME multipart MJPEG framing (`--frame\r\n...`). Sets `TCP_NODELAY=true` + `sendBufferSize=524288` after accept. Sets `outputStream=null` on send failure (disconnect detection).

---

### Linux (`linux/` — Cargo crate)

- **`src/main.rs`** — egui `CamPCApp`. Catppuccin Mocha theme. Top panel: status. Bottom panel: FPS slider (5-30), Rotation buttons (0/90/180/270°), Connection mode (WiFi/USB), WiFi IP field, Zoom slider, Iniciar/Detener/Salir. Central panel: 640×360 preview canvas (GPU texture, bilinear). `on_exit()` kills FFmpeg by PID synchronously + removes ADB forward.

- **`src/engine.rs`** — Engine thread state machine: `Idle → WaitingDevice → Connecting → Streaming`. Polls `adb device_connected()` every 2s (USB) or watches beacon expiry (WiFi). Spawns FFmpeg; checks health every 200ms; respawns on exit (500ms delay). `EngineCmd`: `Start`, `Stop`, `UpdateConfig(Config)`.

- **`src/ffmpeg.rs`** — FFmpeg supervisor + frame reader. `build_vf_filter(cfg)`: `[transpose] → crop=iw:iw*9/16 → scale=1280:720:in_range=full:out_range=limited → setsar=1`. Frame-reader thread: reads yuv420p frames → `V4l2Writer::write_frame()` → `yuv420p_to_preview_rgb()` → `preview_tx`. `kill_pid(pid)` uses `kill {pid}`.

- **`src/v4l2.rs`** — `V4l2Writer`: opens device, tries `VIDIOC_S_FMT` with `VIDEO_OUTPUT=2` first and falls back to `VIDEO_CAPTURE=1`, then writes frames via `write_all()`.

- **`src/adb.rs`** — `device_connected()`, `forward(port)`, `remove_forward(port)`.

- **`src/discovery.rs`** — UDP listener on `:5001`; updates shared `DiscoveredDevice` when `CAMPC_HELLO` beacon arrives. 5s timeout before device considered lost.

- **`src/config.rs`** — `Config { fps, rotation, v4l2_device, adb_port, preview_fps, connection_mode, wifi_ip, zoom }`. Loads/saves TOML at `~/.config/campc/config.toml`. Defaults: `connection_mode=Wifi`, `zoom=1.0`.

---

### Windows (`windows/` — Cargo crate)

Mirrors `linux/` with these differences:

- **`src/mf/`** — Replaces `v4l2.rs`. MediaFoundation virtual camera split across 5 files:
  - `mod.rs` — `VirtualCamWriter` public API: `new(w, h)` calls `MFStartup`, pre-creates stream event queue (avoids re-entrant MFPlat deadlock), builds NV12 media type + stream/presentation descriptors, constructs `AndroidCamSource`, dynamically loads `MFCreateVirtualCamera` from `mfsensorgroup.dll`, calls `add_media_source` + `start` on the raw vtable handle. `write_frame(nv12)`: if pending `RequestSample` token → fires `MEMediaSample` event; else stores frame for next poll. `Drop`: calls vtable `Remove()` + `MFShutdown()`.
  - `camera.rs` — `VirtualCamHandle`: raw COM vtable dispatch wrapper. `MFCreateVirtualCamera` loaded via `GetProcAddress` from `mfsensorgroup.dll` (windows-rs 0.58 bindings are incorrect for this function). Vtable slots: 2=Release, 3=AddMediaSource, 4=Start, 6=Remove.
  - `source.rs` — `AndroidCamSource` implements `IMFMediaSourceEx + IMFMediaEventGenerator`. `GetCharacteristics` returns `MFMEDIASOURCE_DOES_NOT_USE_NETWORK`. `Start()`/`Stop()` manage stream state and fire `MESourceStarted`/`MESourceStopped` events.
  - `stream.rs` — `AndroidCamStream` implements `IMFMediaStream + IMFMediaEventGenerator`. `RequestSample()`: if a frame is ready → `QueueEventParamUnk(MEMediaSample)`; else enqueues the token for `write_frame()` to fulfil.
  - `constants.rs` — GUID constants for `MF_DEVICESTREAM_STREAM_ID`, `MF_DEVICESTREAM_STREAM_CATEGORY`, `MF_DEVICESTREAM_FRAMESOURCE_TYPES`, `PINNAME_VIDEO_CAPTURE`, `MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE`, etc.
  - `types.rs` — `StreamShared` (Arc'd state between VirtualCamWriter and AndroidCamStream: pending tokens deque, latest frame, event queue ref, running flag). `build_nv12_media_type()`. `build_sample()`.

- **`src/ffmpeg/`** — FFmpeg supervision split across 4 files:
  - `mod.rs` — Re-exports `kill`, `kill_pid`, `spawn_ffmpeg`. Defines `PREVIEW_W/H/FRAME_BYTES`, `OUTPUT_W/H`.
  - `filter.rs` — `build_vf_filter(cfg)` builds the `-vf` string.
  - `process.rs` — `spawn_ffmpeg()`, `kill()`, `kill_pid()` (uses `taskkill /F /PID`).
  - `reader.rs` — Frame-reader thread: initialises COM via `ComInitGuard` (from `mf` module), reads yuv420p from FFmpeg stdout → `yuv420p_to_nv12()` → `VirtualCamWriter::write_frame()` → `yuv420p_to_preview_rgb()` → `preview_tx.try_send()`.

- **`src/ui/`** — UI helpers extracted from `main.rs`:
  - `theme.rs` — `apply_theme()` applies Catppuccin Mocha visuals to an `egui::Context`.
  - `widgets.rs` — `action_btn()` and `selectable_btn()` reusable egui widget builders.

- **`src/pixel_fmt.rs`** — `yuv420p_to_nv12()` and `yuv420p_to_preview_rgb()`.

- **`src/config.rs`** — No `v4l2_device` field. Config path: `%APPDATA%\campc\config.toml`.

- **`src/main.rs`** — Adds `init_logger()` (writes to `%APPDATA%\campc\campc.log` via `simplelog`). Uses `mod mf`, `mod ffmpeg`, `mod ui`. UI split into `render_header()`, `render_controls()`, `render_preview_canvas()` methods on `CamPCApp`.

- **`src/engine.rs`**, **`src/adb.rs`**, **`src/discovery.rs`** — Identical to Linux (cross-platform).

- **`Cargo.toml`** — No `libc`, no `x11` eframe feature. Adds `windows = { version = "0.58", features = [...MediaFoundation, Com, Foundation, Com_StructuredStorage, Variant, Audio, LibraryLoader, implement] }`, `windows-core = "0.58"`, `log = "0.4"`, `simplelog = "0.12"`.

---

## Threading model (Rust — same on both platforms)

```
main thread        egui event loop → repaints at 30fps, drains preview_rx channel
engine thread      ADB/beacon poll + FFmpeg spawn/health/respawn loop (200ms tick)
frame-reader       spawned per FFmpeg instance; reads stdout pipe → virtual cam write + preview_tx send
discovery thread   UDP :5001 listener; updates DiscoveredDevice on CAMPC_HELLO beacon
```

---

## Key implementation notes

### Linux — V4L2 with exclusive_caps=1
`V4l2Writer` tries `VIDIOC_S_FMT` with `V4L2_BUF_TYPE_VIDEO_OUTPUT=2` first (v4l2loopback 0.13+ on kernel 6.x), falls back to `V4L2_BUF_TYPE_VIDEO_CAPTURE=1` for older versions. Without `exclusive_caps=1`, Zoom/Meet/Teams won't recognize the device as a real capture camera.

### Windows — IMFVirtualCamera requirements
Requires Windows 11 22H2+ (Build 22621+) **and Developer Mode enabled** (Settings → System → For developers). No third-party drivers needed. The virtual camera appears as "AndroidCam" in Settings → Privacy → Camera and in video conferencing app device lists.

`MFCreateVirtualCamera` is loaded at runtime via `GetProcAddress` from `mfsensorgroup.dll` because the windows-rs 0.58 static bindings for this function are incorrect (wrong signature). The `sourceId` parameter must be a non-null GUID string — passing `null` produces `E_INVALIDARG (0x80070057)`.

### Windows — File logging
`init_logger()` in `main.rs` writes to `%APPDATA%\campc\campc.log` (overwritten each run) using `simplelog`. All modules use `log::info!/warn!/error!` macros. Check this file first when diagnosing failures.

### Windows — Stream event queue pre-creation
`StreamShared::new()` creates the `IMFMediaEventQueue` **before** `IMFVirtualCamera::Start()` is called. Creating it inside `AndroidCamSource::Start()` would re-enter mfplat while the virtual camera holds an internal lock, causing an access violation in `mfplat.dll`.

### Windows — NV12 pixel format
`IMFVirtualCamera` expects NV12 (semi-planar). FFmpeg outputs yuv420p (planar). `yuv420p_to_nv12()` in `ffmpeg.rs` interleaves the U and V planes: Y plane is copied as-is; UV plane pairs each U byte with the corresponding V byte.

### CameraX resolution — why FALLBACK_RULE_CLOSEST_LOWER
`FALLBACK_RULE_CLOSEST_LOWER` keeps CameraX biased toward 16:9 modes around the requested 1280×720 instead of jumping to a higher 4:3 candidate.

### FFmpeg low-latency flags
`-probesize 32 -analyzeduration 0` eliminate the ~1-2s startup buffer. `-fflags nobuffer -flags low_delay` reduce per-frame buffering. Without these, FFmpeg buffers ~5MB before outputting the first frame.

### ADB port in engine.rs
`UpdateConfig` saves `old_port = config.adb_port` **before** overwriting `config = new_cfg`, then calls `adb::remove_forward(old_port)`. The current code does this correctly.

### After kernel upgrade (Linux)
Re-run `setup_ubuntu.sh` — it rebuilds and reinstalls `v4l2loopback` for the new kernel.

---

## Differences Linux vs Windows (quick reference)

| Component | Linux | Windows |
|---|---|---|
| Virtual camera | `/dev/video10` via ioctl (v4l2.rs) | `IMFVirtualCamera` COM (mf/ module) |
| Pixel format written | YUV420P planar | NV12 semi-planar |
| Kill process | `kill {pid}` | `taskkill /F /PID {pid}` |
| Config path | `~/.config/campc/config.toml` | `%APPDATA%\campc\config.toml` |
| Config field | `v4l2_device: String` | _(absent)_ |
| eframe features | `glow`, `x11` | `glow` |
| Extra deps | `libc` | `windows-rs 0.58` |

---

## What does NOT exist (legacy, removed, or never built)
- `linux/campc.py` — Python/Tkinter prototype. No longer the primary app.
- `linux/launch.sh` / `linux/setup.sh` — Legacy scripts for Fedora/ffmpeg workflow.
- Zoom, resolution, or quality controls in the GUI — removed; output is fixed 1280×720, JPEG quality is fixed on Android.
