use std::io::{BufReader, Read};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::thread;

use crate::config::Config;
use crate::v4l2::V4l2Writer;

// Preview stream dimensions shown in the GUI canvas (RGB24)
pub const PREVIEW_W: u32 = 640;
pub const PREVIEW_H: u32 = 360;
pub const PREVIEW_FRAME_BYTES: usize = (PREVIEW_W * PREVIEW_H * 3) as usize;

// Fixed output resolution — always 1080p for maximum quality
pub const OUTPUT_W: u32 = 1920;
pub const OUTPUT_H: u32 = 1080;

/// Builds a `-vf` filter string from the current config.
///
/// Applies rotation, aspect-ratio crop, and scale to fixed 1080p output.
/// Single output — no split needed since V4L2 writing is done in Rust.
pub fn build_vf_filter(cfg: &Config) -> String {
    let mut steps: Vec<String> = Vec::new();

    // 1. Rotation (transpose avoids quality loss vs. rotate filter)
    match cfg.rotation {
        90 => steps.push("transpose=1".to_string()),  // 90° CW
        180 => steps.push("hflip,vflip".to_string()), // 180°
        270 => steps.push("transpose=2".to_string()), // 90° CCW
        _ => {}
    }

    // 2. Crop to 16:9 aspect ratio.
    // The phone may stream a non-16:9 frame. Without this step FFmpeg
    // stretches it and sets SAR≠1, causing distortion in apps that ignore SAR.
    steps.push(format!(
        "crop=iw:iw*{OUTPUT_H}/{OUTPUT_W}:0:(ih-iw*{OUTPUT_H}/{OUTPUT_W})/2"
    ));

    // 3. Scale to 1920×1080.
    // in_range=full   — input is JPEG/full-range (yuvj420p, Y 0-255)
    // out_range=limited — standard TV range expected by V4L2 consumers (Y 16-235)
    // flags=lanczos   — higher-quality resampling vs. default bilinear
    steps.push(format!(
        "scale={OUTPUT_W}:{OUTPUT_H}:in_range=full:out_range=limited"
    ));

    // 4. Ensure square pixels (SAR 1:1) so consuming apps display correctly.
    steps.push("setsar=1".to_string());

    steps.join(",")
}

/// Spawns FFmpeg and returns (Child, pid), or None on failure.
///
/// `tcp_host` and `tcp_port` identify the MJPEG source:
///   - USB mode: `("localhost", adb_port)` — tunnelled through ADB forward.
///   - WiFi mode: `("192.168.x.x", 5000)` — direct connection to the phone.
///
/// FFmpeg outputs a single rawvideo yuv420p stream to stdout.
/// A reader thread reads frames, writes them to the V4L2 device via Rust
/// ioctls (bypassing FFmpeg's v4l2 muxer which fails on kernel 6.17), and
/// generates downscaled RGB24 previews for the GUI.
pub fn spawn_ffmpeg(cfg: &Config, tcp_host: &str, tcp_port: u16, preview_tx: SyncSender<Vec<u8>>) -> Option<(Child, u32)> {
    let vf = build_vf_filter(cfg);
    let tcp_url = format!("tcp://{}:{}", tcp_host, tcp_port);
    let fps_str = cfg.fps.to_string();
    let device = cfg.v4l2_device.clone();
    let (out_w, out_h) = (OUTPUT_W, OUTPUT_H);

    let args: Vec<String> = vec![
        "-hide_banner".into(),
        // Low-latency flags
        "-fflags".into(), "nobuffer".into(),
        "-flags".into(), "low_delay".into(),
        "-probesize".into(), "32".into(),
        "-analyzeduration".into(), "0".into(),
        // Allow FFmpeg's thread queue to buffer enough packets before the
        // decoder starts consuming them — prevents starvation under WiFi jitter.
        "-thread_queue_size".into(), "512".into(),
        // Explicitly specify multipart-JPEG format — no stream probing needed.
        "-f".into(), "mpjpeg".into(),
        "-i".into(), tcp_url,
        // Video filter: zoom → rotate → scale
        "-vf".into(), vf,
        // Single output: raw yuv420p frames to stdout.
        // Rust reads these and writes to /dev/videoN via VIDIOC_S_FMT + write(),
        // which works on v4l2loopback even when VIDIOC_G_FMT returns EINVAL.
        "-f".into(), "rawvideo".into(),
        "-pix_fmt".into(), "yuv420p".into(),
        "-r".into(), fps_str,
        "pipe:1".into(),
    ];

    let mut child = Command::new("ffmpeg")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .ok()?;

    let pid = child.id();
    let stdout = child.stdout.take()?;
    spawn_frame_reader(stdout, preview_tx, device, out_w, out_h, cfg.fps, cfg.preview_fps);
    Some((child, pid))
}

/// Reads yuv420p frames from `stdout`, writes each frame to the V4L2 device,
/// and sends downscaled RGB24 thumbnails to `preview_tx` for the GUI.
///
/// `v4l2_fps` is the full output frame rate; `preview_fps` is the desired GUI
/// refresh rate (≤ v4l2_fps). Every N-th frame is converted and offered to the
/// channel. If the channel buffer is full the preview frame is dropped (the GUI
/// is simply not updated that tick) — V4L2 always receives every frame.
fn spawn_frame_reader(
    stdout: ChildStdout,
    preview_tx: SyncSender<Vec<u8>>,
    device: String,
    out_w: u32,
    out_h: u32,
    v4l2_fps: u32,
    preview_fps: u32,
) {
    thread::spawn(move || {
        let frame_bytes = (out_w * out_h * 3 / 2) as usize;
        let mut reader = BufReader::with_capacity(frame_bytes, stdout);
        let mut yuv_buf = vec![0u8; frame_bytes];

        // Open the V4L2 device once and reuse across frames.
        let mut v4l2 = V4l2Writer::new(&device, out_w, out_h);
        if v4l2.is_none() {
            eprintln!("[ffmpeg] V4L2 device unavailable — preview only");
        }

        // Preview throttle: send a GUI frame every `preview_every` V4L2 frames.
        // If preview_fps >= v4l2_fps there is no throttle (every frame sent).
        let preview_every = if preview_fps >= v4l2_fps {
            1usize
        } else {
            (v4l2_fps / preview_fps.max(1)).max(1) as usize
        };
        let mut frame_count: usize = 0;

        loop {
            match reader.read_exact(&mut yuv_buf) {
                Ok(()) => {
                    // Always write the full-resolution frame to V4L2.
                    if let Some(ref mut writer) = v4l2 {
                        if !writer.write_frame(&yuv_buf) {
                            eprintln!("[ffmpeg] V4L2 write error — retrying open");
                            v4l2 = V4l2Writer::new(&device, out_w, out_h);
                        }
                    }

                    // Throttled: only convert and send a preview every N frames.
                    frame_count += 1;
                    if frame_count % preview_every == 0 {
                        let preview = yuv420p_to_preview_rgb(&yuv_buf, out_w, out_h);
                        match preview_tx.try_send(preview) {
                            // Sent OK, or GUI is temporarily slow — drop this frame.
                            Ok(()) | Err(TrySendError::Full(_)) => {}
                            // Receiver dropped — GUI is gone, stop the thread.
                            Err(TrySendError::Disconnected(_)) => break,
                        }
                    }
                }
                Err(_) => break, // pipe closed → FFmpeg exited
            }
        }
    });
}

/// Downsample a yuv420p frame to PREVIEW_W×PREVIEW_H and convert to RGB24.
///
/// Uses nearest-neighbour sampling (fast; preview is small so quality is fine).
fn yuv420p_to_preview_rgb(yuv: &[u8], src_w: u32, src_h: u32) -> Vec<u8> {
    let mut rgb = vec![0u8; PREVIEW_FRAME_BYTES];

    let y_plane = &yuv[..(src_w * src_h) as usize];
    let u_plane = &yuv[(src_w * src_h) as usize..(src_w * src_h * 5 / 4) as usize];
    let v_plane = &yuv[(src_w * src_h * 5 / 4) as usize..];

    // Integer scale factors (floor division — preview covers the top-left portion)
    let scale_x = (src_w / PREVIEW_W).max(1);
    let scale_y = (src_h / PREVIEW_H).max(1);

    for py in 0..PREVIEW_H {
        let sy = py * scale_y;
        for px in 0..PREVIEW_W {
            let sx = px * scale_x;

            // ITU-R BT.601 limited-range YCbCr → RGB (integer approximation):
            //   Y'  in [16, 235]  → rescale to [0, 255]:  y = (Y' − 16) × 255/219
            //   Cb, Cr centred at 128:  u = Cb − 128,  v = Cr − 128
            //
            // Matrix coefficients (×1000 to stay in integer arithmetic):
            //   R = Y + 1.402  × Cr   →  y + 1402 × v / 1000
            //   G = Y − 0.344  × Cb   →  y −  344 × u / 1000
            //       − 0.714  × Cr         −  714 × v / 1000
            //   B = Y + 1.772  × Cb   →  y + 1772 × u / 1000
            let y = (((y_plane[(sy * src_w + sx) as usize] as i32) - 16) * 255 / 219).clamp(0, 255);
            let u = u_plane[((sy / 2) * (src_w / 2) + (sx / 2)) as usize] as i32 - 128;
            let v = v_plane[((sy / 2) * (src_w / 2) + (sx / 2)) as usize] as i32 - 128;

            let r = (y + 1402 * v / 1000).clamp(0, 255) as u8;
            let g = (y -  344 * u / 1000 - 714 * v / 1000).clamp(0, 255) as u8;
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

/// Kill a process by raw PID (used by on_exit to kill orphaned FFmpeg).
pub fn kill_pid(pid: u32) {
    let _ = Command::new("kill").arg(pid.to_string()).status();
}
