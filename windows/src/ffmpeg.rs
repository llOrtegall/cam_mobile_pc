use std::io::{BufReader, Read};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::thread;

use log::warn;

use crate::config::Config;
use crate::virtual_cam::VirtualCamWriter;

// Preview stream dimensions shown in the GUI canvas (RGB24)
pub const PREVIEW_W: u32 = 640;
pub const PREVIEW_H: u32 = 360;
pub const PREVIEW_FRAME_BYTES: usize = (PREVIEW_W * PREVIEW_H * 3) as usize;

// Fixed output resolution — 720p for lower latency and bandwidth
pub const OUTPUT_W: u32 = 1280;
pub const OUTPUT_H: u32 = 720;

/// Builds a `-vf` filter string from the current config.
///
/// Applies rotation, aspect-ratio crop, and scale to fixed 720p output.
pub fn build_vf_filter(cfg: &Config) -> String {
    let mut steps: Vec<String> = Vec::new();

    // 0. Normalise the deprecated yuvj420p pixel format (JPEG full-range)
    //    to yuv420p + explicit full-range metadata so swscaler never sees
    //    the deprecated "j" variant and the range conversion in step 3 is
    //    applied correctly.
    steps.push("setrange=full".to_string());

    // 1. Rotation (transpose avoids quality loss vs. rotate filter)
    match cfg.rotation {
        90 => steps.push("transpose=1".to_string()),  // 90° CW
        180 => steps.push("hflip,vflip".to_string()), // 180°
        270 => steps.push("transpose=2".to_string()), // 90° CCW
        _ => {}
    }

    // 2. Crop to 16:9 aspect ratio.
    steps.push(format!(
        "crop=iw:iw*{OUTPUT_H}/{OUTPUT_W}:0:(ih-iw*{OUTPUT_H}/{OUTPUT_W})/2"
    ));

    // 3. Scale to fixed 1280×720 output.
    // in_range=full   — input is JPEG/full-range (yuvj420p, Y 0-255)
    // out_range=limited — standard TV range expected by virtual camera consumers
    steps.push(format!(
        "scale={OUTPUT_W}:{OUTPUT_H}:in_range=full:out_range=limited"
    ));

    // 4. Ensure square pixels (SAR 1:1) so consuming apps display correctly.
    steps.push("setsar=1".to_string());

    steps.join(",")
}

fn build_ffmpeg_args(cfg: &Config, tcp_url: &str, vf: &str) -> Vec<String> {
    let fps_str = cfg.fps.to_string();
    vec![
        "-hide_banner".into(),
        // Low-latency flags.
        "-fflags".into(),
        "nobuffer".into(),
        "-flags".into(),
        "low_delay".into(),
        "-probesize".into(),
        "32".into(),
        "-analyzeduration".into(),
        "0".into(),
        // Absorb short network jitter bursts before decoder consumption.
        "-thread_queue_size".into(),
        "64".into(),
        // Input: multipart MJPEG stream from phone.
        "-f".into(),
        "mpjpeg".into(),
        "-i".into(),
        tcp_url.to_string(),
        "-vf".into(),
        vf.to_string(),
        // Output: raw yuv420p frames to stdout for Rust virtual camera writer.
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-r".into(),
        fps_str,
        "pipe:1".into(),
    ]
}

/// Spawns FFmpeg and returns (Child, pid), or None on failure.
///
/// `tcp_host` and `tcp_port` identify the MJPEG source:
///   - USB mode: `("localhost", adb_port)` — tunnelled through ADB forward.
///   - WiFi mode: `("192.168.x.x", 5000)` — direct connection to the phone.
pub fn spawn_ffmpeg(
    cfg: &Config,
    tcp_host: &str,
    tcp_port: u16,
    preview_tx: SyncSender<Vec<u8>>,
) -> Option<(Child, u32)> {
    let vf = build_vf_filter(cfg);
    let tcp_url = format!("tcp://{}:{}", tcp_host, tcp_port);
    let args = build_ffmpeg_args(cfg, &tcp_url, &vf);
    let (out_w, out_h) = (OUTPUT_W, OUTPUT_H);

    let mut child = Command::new("ffmpeg")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .ok()?;

    let pid = child.id();
    let stdout = child.stdout.take()?;
    spawn_frame_reader(stdout, preview_tx, out_w, out_h, cfg.fps, cfg.preview_fps);
    Some((child, pid))
}

/// Reads yuv420p frames from `stdout`, converts to NV12 and writes each frame
/// to the IMFVirtualCamera, and sends downscaled RGB24 thumbnails to
/// `preview_tx` for the GUI.
fn spawn_frame_reader(
    stdout: ChildStdout,
    preview_tx: SyncSender<Vec<u8>>,
    out_w: u32,
    out_h: u32,
    v4l2_fps: u32,
    preview_fps: u32,
) {
    thread::spawn(move || {
        // IMFVirtualCamera requires COM to be initialised on the thread that
        // calls its methods. We use COINIT_MULTITHREADED so the MF work queue
        // threads can also access the COM objects safely.
        let _com_guard = ComInitGuard::new();

        let frame_bytes = (out_w * out_h * 3 / 2) as usize;
        let mut reader = BufReader::with_capacity(frame_bytes, stdout);
        let mut yuv_buf = vec![0u8; frame_bytes];

        // Open the virtual camera once and reuse across frames.
        let mut vcam = VirtualCamWriter::new(out_w, out_h);
        if vcam.is_none() {
            warn!("[ffmpeg] VirtualCamera unavailable — preview only");
        }

        // Preview throttle: send a GUI frame every `preview_every` output frames.
        let preview_every = if preview_fps >= v4l2_fps {
            1usize
        } else {
            (v4l2_fps / preview_fps.max(1)).max(1) as usize
        };
        let mut frame_count: usize = 0;

        while let Ok(()) = reader.read_exact(&mut yuv_buf) {
            // Convert yuv420p (planar) → NV12 (semi-planar) for MediaFoundation.
            let nv12 = yuv420p_to_nv12(&yuv_buf, out_w, out_h);

            // Write the full-resolution frame to the virtual camera.
            if let Some(ref mut writer) = vcam {
                if !writer.write_frame(&nv12) {
                    warn!("[ffmpeg] VirtualCamera write error — retrying open");
                    vcam = VirtualCamWriter::new(out_w, out_h);
                }
            }

            // Throttle preview work so UI conversion/upload stays cheap.
            frame_count += 1;
            if frame_count % preview_every == 0 {
                let preview = yuv420p_to_preview_rgb(&yuv_buf, out_w, out_h);
                match preview_tx.try_send(preview) {
                    Ok(()) | Err(TrySendError::Full(_)) => {}
                    Err(TrySendError::Disconnected(_)) => break,
                }
            }
        }
    });
}

/// Convert yuv420p (planar: Y, U, V) → NV12 (semi-planar: Y, UV interleaved).
///
/// Y plane is identical. UV plane interleaves U and V bytes.
pub fn yuv420p_to_nv12(yuv: &[u8], w: u32, h: u32) -> Vec<u8> {
    let y_size = (w * h) as usize;
    let uv_size = y_size / 2; // total UV bytes in NV12

    let mut nv12 = vec![0u8; y_size + uv_size];

    // Y plane: copy as-is.
    nv12[..y_size].copy_from_slice(&yuv[..y_size]);

    // UV plane: interleave U and V from the planar source.
    let u_plane = &yuv[y_size..y_size + uv_size / 2];
    let v_plane = &yuv[y_size + uv_size / 2..];

    for i in 0..uv_size / 2 {
        nv12[y_size + i * 2]     = u_plane[i];
        nv12[y_size + i * 2 + 1] = v_plane[i];
    }

    nv12
}

/// Downsample a yuv420p frame to PREVIEW_W×PREVIEW_H and convert to RGB24.
///
/// Uses nearest-neighbour sampling (fast; preview is small so quality is fine).
fn yuv420p_to_preview_rgb(yuv: &[u8], src_w: u32, src_h: u32) -> Vec<u8> {
    let mut rgb = vec![0u8; PREVIEW_FRAME_BYTES];

    let y_plane = &yuv[..(src_w * src_h) as usize];
    let u_plane = &yuv[(src_w * src_h) as usize..(src_w * src_h * 5 / 4) as usize];
    let v_plane = &yuv[(src_w * src_h * 5 / 4) as usize..];

    let scale_x = (src_w / PREVIEW_W).max(1);
    let scale_y = (src_h / PREVIEW_H).max(1);

    for py in 0..PREVIEW_H {
        let sy = py * scale_y;
        for px in 0..PREVIEW_W {
            let sx = px * scale_x;

            // Convert limited-range YUV (BT.601) to RGB using integer math.
            let y = (((y_plane[(sy * src_w + sx) as usize] as i32) - 16) * 255 / 219).clamp(0, 255);
            let u = u_plane[((sy / 2) * (src_w / 2) + (sx / 2)) as usize] as i32 - 128;
            let v = v_plane[((sy / 2) * (src_w / 2) + (sx / 2)) as usize] as i32 - 128;

            let r = (y + 1402 * v / 1000).clamp(0, 255) as u8;
            let g = (y - 344 * u / 1000 - 714 * v / 1000).clamp(0, 255) as u8;
            let b = (y + 1772 * u / 1000).clamp(0, 255) as u8;

            let idx = ((py * PREVIEW_W + px) * 3) as usize;
            rgb[idx] = r;
            rgb[idx + 1] = g;
            rgb[idx + 2] = b;
        }
    }

    rgb
}

/// Kills the FFmpeg child process and waits for it to exit.
pub fn kill(proc: &mut Option<Child>) {
    if let Some(mut child) = proc.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Kill a process by raw PID using taskkill (used by on_exit to kill orphaned FFmpeg).
pub fn kill_pid(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .status();
}

// ── COM initialisation guard ──────────────────────────────────────────────────

/// RAII guard that calls CoInitializeEx on construction and CoUninitialize on drop.
struct ComInitGuard;

impl ComInitGuard {
    fn new() -> Self {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
        // COINIT_MULTITHREADED: S_OK (first init) or S_FALSE (already initialised
        // with same model) are both fine. RPC_E_CHANGED_MODE means another model
        // is active — tolerated since we only write samples, not marshal objects.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        ComInitGuard
    }
}

impl Drop for ComInitGuard {
    fn drop(&mut self) {
        use windows::Win32::System::Com::CoUninitialize;
        unsafe { CoUninitialize() };
    }
}
