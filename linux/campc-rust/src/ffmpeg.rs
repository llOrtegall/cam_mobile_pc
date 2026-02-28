use std::io::{BufReader, Read};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;

use crate::config::Config;

// Preview stream dimensions sent from FFmpeg via stdout (rawvideo rgb24)
pub const PREVIEW_W: u32 = 320;
pub const PREVIEW_H: u32 = 180;
pub const PREVIEW_FRAME_BYTES: usize = (PREVIEW_W * PREVIEW_H * 3) as usize;

/// Builds a FFmpeg filter_complex string from the current config.
///
/// The filter produces two named outputs:
///   [v4l2out]  — full-resolution, ready to write to /dev/video10
///   [preview]  — PREVIEW_W×PREVIEW_H for the GUI canvas
pub fn build_filter_complex(cfg: &Config) -> String {
    let (out_w, out_h) = cfg.resolution_dims();
    let mut steps: Vec<String> = Vec::new();

    // 1. Centre-crop for zoom (1.0 = no crop)
    if cfg.zoom > 1.001 {
        steps.push(format!(
            "crop=iw/{z}:ih/{z}:(iw-iw/{z})/2:(ih-ih/{z})/2",
            z = cfg.zoom
        ));
    }

    // 2. Rotation via transpose (FFmpeg's transpose avoids quality loss)
    match cfg.rotation {
        90 => steps.push("transpose=1".to_string()),           // 90° CW
        180 => steps.push("hflip,vflip".to_string()),          // 180°
        270 => steps.push("transpose=2".to_string()),          // 90° CCW
        _ => {}
    }

    // 3. Scale to target output resolution
    steps.push(format!("scale={out_w}:{out_h}"));

    let chain = if steps.is_empty() {
        "null".to_string()
    } else {
        steps.join(",")
    };

    // Split into two outputs: full-res for V4L2 + small thumbnail for GUI
    format!(
        "[0:v]{chain}[processed];\
         [processed]split=2[v4l2out][prevtemp];\
         [prevtemp]scale={pw}:{ph}[preview]",
        chain = chain,
        pw = PREVIEW_W,
        ph = PREVIEW_H,
    )
}

/// Spawns FFmpeg with:
///   - Low-latency input flags (no probe/analysis buffering)
///   - filter_complex built from `cfg`
///   - Output 1: -f v4l2 → /dev/video10
///   - Output 2: rawvideo rgb24 → stdout (preview)
///
/// Spawns a background reader thread that pushes frames to `preview_tx`.
/// Returns the Child handle, or None if the spawn failed.
pub fn spawn_ffmpeg(cfg: &Config, preview_tx: Sender<Vec<u8>>) -> Option<Child> {
    let filter = build_filter_complex(cfg);
    let tcp_url = format!("tcp://localhost:{}", cfg.adb_port);
    let fps_str = cfg.fps.to_string();
    let preview_fps_str = cfg.preview_fps.to_string();
    let device = cfg.v4l2_device.clone();

    // Build arg list as owned Strings first so references below are valid
    let args: Vec<String> = vec![
        // Suppress the startup banner
        "-hide_banner".into(),
        // Low-latency flags: skip stream analysis to avoid initial buffering
        "-fflags".into(), "nobuffer".into(),
        "-flags".into(), "low_delay".into(),
        "-probesize".into(), "32".into(),
        "-analyzeduration".into(), "0".into(),
        // Input: MJPEG over TCP from ADB forward
        "-i".into(), tcp_url,
        // Video processing
        "-filter_complex".into(), filter,
        // ── Output 1: v4l2loopback ──
        "-map".into(), "[v4l2out]".into(),
        "-f".into(), "v4l2".into(),
        "-pix_fmt".into(), "yuv420p".into(),
        "-r".into(), fps_str,
        device,
        // ── Output 2: preview via stdout ──
        "-map".into(), "[preview]".into(),
        "-f".into(), "rawvideo".into(),
        "-pix_fmt".into(), "rgb24".into(),
        "-r".into(), preview_fps_str,
        "pipe:1".into(),
    ];

    let mut child = Command::new("ffmpeg")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null()) // suppress FFmpeg log noise; set to Stdio::inherit() to debug
        .spawn()
        .ok()?;

    let stdout = child.stdout.take()?;
    spawn_preview_reader(stdout, preview_tx);
    Some(child)
}

/// Reads fixed-size rawvideo rgb24 frames from `stdout` and forwards them
/// to `tx`. Runs in its own thread; exits when the pipe closes (FFmpeg exit).
fn spawn_preview_reader(stdout: ChildStdout, tx: Sender<Vec<u8>>) {
    thread::spawn(move || {
        let mut reader = BufReader::with_capacity(PREVIEW_FRAME_BYTES * 4, stdout);
        let mut buf = vec![0u8; PREVIEW_FRAME_BYTES];
        loop {
            match reader.read_exact(&mut buf) {
                Ok(()) => {
                    if tx.send(buf.clone()).is_err() {
                        break; // GUI dropped the receiver — time to quit
                    }
                }
                Err(_) => break, // pipe closed → FFmpeg exited
            }
        }
    });
}

/// Kills the FFmpeg child process and waits for it to exit.
pub fn kill(proc: &mut Option<Child>) {
    if let Some(mut child) = proc.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}
