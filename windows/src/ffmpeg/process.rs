use std::process::{Child, Command, Stdio};
use std::sync::mpsc::SyncSender;

use crate::config::Config;
use super::filter::{build_ffmpeg_args, build_vf_filter};
use super::reader::spawn_frame_reader;
use super::{OUTPUT_H, OUTPUT_W};

/// Spawns FFmpeg and returns (Child, pid), or None on failure.
///
/// `tcp_host` and `tcp_port` identify the MJPEG source:
/// - USB mode: `(localhost, adb_port)` via ADB forward.
/// - WiFi mode: `(phone_ip, 5000)` direct.
pub fn spawn_ffmpeg(
    cfg: &Config,
    tcp_host: &str,
    tcp_port: u16,
    preview_tx: SyncSender<Vec<u8>>,
) -> Option<(Child, u32)> {
    let vf = build_vf_filter(cfg);
    let tcp_url = format!("tcp://{}:{}", tcp_host, tcp_port);
    let args = build_ffmpeg_args(cfg, &tcp_url, &vf);

    let mut child = Command::new("ffmpeg")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .ok()?;

    let pid = child.id();
    let stdout = child.stdout.take()?;
    spawn_frame_reader(stdout, preview_tx, OUTPUT_W, OUTPUT_H, cfg.fps, cfg.preview_fps);
    Some((child, pid))
}

/// Kills the FFmpeg child process and waits for it to exit.
pub fn kill(proc: &mut Option<Child>) {
    if let Some(mut child) = proc.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Kill a process by raw PID using taskkill (used by on_exit for orphan cleanup).
pub fn kill_pid(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .status();
}
