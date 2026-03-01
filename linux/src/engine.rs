use std::process::Child;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::{adb, config::Config, ffmpeg};

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Idle,
    WaitingDevice,
    Connecting,
    Streaming,
    Error(String),
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::Idle => write!(f, "Detenido"),
            Status::WaitingDevice => write!(f, "Esperando dispositivo…"),
            Status::Connecting => write!(f, "Conectando…"),
            Status::Streaming => write!(f, "● Transmitiendo"),
            Status::Error(e) => write!(f, "Error: {e}"),
        }
    }
}

pub enum EngineCmd {
    Start,
    Stop,
    /// Replace current config and, if streaming, kill+respawn FFmpeg.
    UpdateConfig(Config),
}

/// Shared state read by the GUI on every repaint.
pub struct AppState {
    pub status: Status,
}

// ── Engine entry point ────────────────────────────────────────────────────────

pub fn spawn(
    state: Arc<Mutex<AppState>>,
    cmd_rx: Receiver<EngineCmd>,
    preview_tx: Sender<Vec<u8>>,
    initial_config: Config,
    // Shared PID of the current FFmpeg process. on_exit() reads this to kill
    // FFmpeg synchronously even if the engine thread is sleeping.
    ffmpeg_pid: Arc<Mutex<Option<u32>>>,
) {
    thread::spawn(move || run(state, cmd_rx, preview_tx, initial_config, ffmpeg_pid));
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn set_status(state: &Arc<Mutex<AppState>>, s: Status) {
    if let Ok(mut st) = state.lock() {
        st.status = s;
    }
}

fn store_pid(ffmpeg_pid: &Arc<Mutex<Option<u32>>>, pid: Option<u32>) {
    if let Ok(mut p) = ffmpeg_pid.lock() {
        *p = pid;
    }
}

// ── Engine loop ───────────────────────────────────────────────────────────────

fn run(
    state: Arc<Mutex<AppState>>,
    cmd_rx: Receiver<EngineCmd>,
    preview_tx: Sender<Vec<u8>>,
    initial_config: Config,
    ffmpeg_pid: Arc<Mutex<Option<u32>>>,
) {
    let mut config = initial_config;
    let mut ffmpeg_proc: Option<Child> = None;
    let mut active = false;
    let mut device_ready = false;

    // Force an immediate ADB check on first Start command
    let mut last_adb_check = Instant::now() - Duration::from_secs(60);

    loop {
        // ── Process all pending commands (non-blocking) ───────────────────────
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                EngineCmd::Start => {
                    active = true;
                    last_adb_check = Instant::now() - Duration::from_secs(60);
                    set_status(&state, Status::WaitingDevice);
                }
                EngineCmd::Stop => {
                    active = false;
                    ffmpeg::kill(&mut ffmpeg_proc);
                    store_pid(&ffmpeg_pid, None);
                    if device_ready {
                        adb::remove_forward(config.adb_port);
                    }
                    device_ready = false;
                    set_status(&state, Status::Idle);
                }
                EngineCmd::UpdateConfig(new_cfg) => {
                    let port_changed = new_cfg.adb_port != config.adb_port;
                    let old_port = config.adb_port;
                    config = new_cfg;
                    if ffmpeg_proc.is_some() {
                        ffmpeg::kill(&mut ffmpeg_proc);
                        store_pid(&ffmpeg_pid, None);
                        set_status(&state, Status::Connecting);
                    }
                    if port_changed && device_ready {
                        adb::remove_forward(old_port);
                        device_ready = false;
                        last_adb_check = Instant::now() - Duration::from_secs(60);
                    }
                }
            }
        }

        if active {
            // ── Periodic ADB device check ─────────────────────────────────────
            if last_adb_check.elapsed() >= Duration::from_secs(2) {
                last_adb_check = Instant::now();

                let connected = adb::device_connected();

                if connected && !device_ready {
                    if adb::forward(config.adb_port) {
                        device_ready = true;
                    } else {
                        set_status(&state, Status::Error("ADB forward falló".to_string()));
                    }
                } else if !connected && device_ready {
                    ffmpeg::kill(&mut ffmpeg_proc);
                    store_pid(&ffmpeg_pid, None);
                    adb::remove_forward(config.adb_port);
                    device_ready = false;
                    set_status(&state, Status::WaitingDevice);
                } else if !connected {
                    set_status(&state, Status::WaitingDevice);
                }
            }

            // ── FFmpeg health check & respawn ─────────────────────────────────
            if device_ready {
                let ffmpeg_exited = ffmpeg_proc
                    .as_mut()
                    .map(|p| p.try_wait().ok().flatten().is_some())
                    .unwrap_or(true);

                if ffmpeg_exited {
                    if ffmpeg_proc.is_some() {
                        ffmpeg::kill(&mut ffmpeg_proc);
                        store_pid(&ffmpeg_pid, None);
                        set_status(&state, Status::Connecting);
                        thread::sleep(Duration::from_millis(500));
                    }

                    set_status(&state, Status::Connecting);

                    match ffmpeg::spawn_ffmpeg(&config, preview_tx.clone()) {
                        Some((proc, pid)) => {
                            store_pid(&ffmpeg_pid, Some(pid));
                            ffmpeg_proc = Some(proc);
                            set_status(&state, Status::Streaming);
                        }
                        None => {
                            store_pid(&ffmpeg_pid, None);
                            set_status(
                                &state,
                                Status::Error("No se pudo iniciar FFmpeg".to_string()),
                            );
                            thread::sleep(Duration::from_secs(2));
                        }
                    }
                }
            }
        }

        thread::sleep(Duration::from_millis(200));
    }
}
