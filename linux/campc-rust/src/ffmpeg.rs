use std::io::{BufReader, Read};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;

use crate::config::Config;
use crate::v4l2::V4l2Writer;

// Preview stream dimensions shown in the GUI canvas (RGB24)
pub const PREVIEW_W: u32 = 320;
pub const PREVIEW_H: u32 = 180;
pub const PREVIEW_FRAME_BYTES: usize = (PREVIEW_W * PREVIEW_H * 3) as usize;

/// Builds a simple `-vf` filter string from the current config.
///
/// Applies centre-crop zoom, rotation, and scale to the target resolution.
/// Single output — no split needed since V4L2 writing is done in Rust.
pub fn build_vf_filter(cfg: &Config) -> String {
    let (out_w, out_h) = cfg.resolution_dims();
    let mut steps: Vec<String> = Vec::new();

    // 1. Centre-crop for zoom
    if cfg.zoom > 1.001 {
        steps.push(format!(
            "crop=iw/{z}:ih/{z}:(iw-iw/{z})/2:(ih-ih/{z})/2",
            z = cfg.zoom
        ));
    }

    // 2. Rotation (transpose avoids quality loss vs. rotate filter)
    match cfg.rotation {
        90 => steps.push("transpose=1".to_string()),  // 90° CW
        180 => steps.push("hflip,vflip".to_string()), // 180°
        270 => steps.push("transpose=2".to_string()), // 90° CCW
        _ => {}
    }

    // 3. Scale to target output resolution
    steps.push(format!("scale={out_w}:{out_h}"));

    if steps.is_empty() {
        "null".to_string()
    } else {
        steps.join(",")
    }
}

/// Spawns FFmpeg and returns (Child, pid), or None on failure.
///
/// FFmpeg outputs a single rawvideo yuv420p stream to stdout.
/// A reader thread reads frames, writes them to the V4L2 device via Rust
/// ioctls (bypassing FFmpeg's v4l2 muxer which fails on kernel 6.17), and
/// generates downscaled RGB24 previews for the GUI.
pub fn spawn_ffmpeg(cfg: &Config, preview_tx: Sender<Vec<u8>>) -> Option<(Child, u32)> {
    let vf = build_vf_filter(cfg);
    let tcp_url = format!("tcp://localhost:{}", cfg.adb_port);
    let fps_str = cfg.fps.to_string();
    let device = cfg.v4l2_device.clone();
    let (out_w, out_h) = cfg.resolution_dims();

    let args: Vec<String> = vec![
        "-hide_banner".into(),
        // Low-latency flags
        "-fflags".into(), "nobuffer".into(),
        "-flags".into(), "low_delay".into(),
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
    spawn_frame_reader(stdout, preview_tx, device, out_w, out_h);
    Some((child, pid))
}

/// Reads yuv420p frames from `stdout`, writes each frame to the V4L2 device,
/// and sends downscaled RGB24 thumbnails to `preview_tx` for the GUI.
fn spawn_frame_reader(
    stdout: ChildStdout,
    preview_tx: Sender<Vec<u8>>,
    device: String,
    out_w: u32,
    out_h: u32,
) {
    thread::spawn(move || {
        let frame_bytes = (out_w * out_h * 3 / 2) as usize;
        let mut reader = BufReader::with_capacity(frame_bytes * 2, stdout);
        let mut yuv_buf = vec![0u8; frame_bytes];

        // Open the V4L2 device once and reuse across frames.
        let mut v4l2 = V4l2Writer::new(&device, out_w, out_h);
        if v4l2.is_none() {
            eprintln!("[ffmpeg] V4L2 device unavailable — preview only");
        }

        loop {
            match reader.read_exact(&mut yuv_buf) {
                Ok(()) => {
                    // Write to V4L2 loopback device
                    if let Some(ref mut writer) = v4l2 {
                        if !writer.write_frame(&yuv_buf) {
                            eprintln!("[ffmpeg] V4L2 write error — retrying open");
                            v4l2 = V4l2Writer::new(&device, out_w, out_h);
                        }
                    }

                    // Generate downscaled RGB24 preview for the GUI
                    let preview = yuv420p_to_preview_rgb(&yuv_buf, out_w, out_h);
                    if preview_tx.send(preview).is_err() {
                        break; // GUI dropped the receiver — we're shutting down
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

            let y = y_plane[(sy * src_w + sx) as usize] as i32;
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

/// Kill a process by raw PID (used by on_exit to kill orphaned FFmpeg).
pub fn kill_pid(pid: u32) {
    let _ = Command::new("kill").arg(pid.to_string()).status();
}
