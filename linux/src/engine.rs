use std::process::Child;
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::{adb, config::{Config, ConnectionMode}, discovery, ffmpeg};

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
    /// IP of the auto-discovered phone (WiFi mode only). None in USB mode or
    /// while no beacon has been received yet.
    pub discovered_ip: Option<String>,
}

// ── Engine entry point ────────────────────────────────────────────────────────

pub fn spawn(
    state: Arc<Mutex<AppState>>,
    cmd_rx: Receiver<EngineCmd>,
    preview_tx: SyncSender<Vec<u8>>,
    initial_config: Config,
    // Shared PID of the current FFmpeg process. on_exit() reads this to kill
    // FFmpeg synchronously even if the engine thread is sleeping.
    ffmpeg_pid: Arc<Mutex<Option<u32>>>,
    discovered: Arc<Mutex<Option<discovery::DiscoveredDevice>>>,
) {
    thread::spawn(move || run(state, cmd_rx, preview_tx, initial_config, ffmpeg_pid, discovered));
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn set_status(state: &Arc<Mutex<AppState>>, s: Status) {
    if let Ok(mut st) = state.lock() {
        st.status = s;
    }
}

fn set_discovered_ip(state: &Arc<Mutex<AppState>>, ip: Option<String>) {
    if let Ok(mut st) = state.lock() {
        st.discovered_ip = ip;
    }
}

fn store_pid(ffmpeg_pid: &Arc<Mutex<Option<u32>>>, pid: Option<u32>) {
    if let Ok(mut p) = ffmpeg_pid.lock() {
        *p = pid;
    }
}

/// Returns the WiFi target IP based on the current config and latest beacon.
/// Manual IP takes priority over auto-discovery.
fn resolve_wifi_ip(
    config: &Config,
    discovered: &Arc<Mutex<Option<discovery::DiscoveredDevice>>>,
) -> Option<String> {
    if !config.wifi_ip.is_empty() {
        return Some(config.wifi_ip.clone());
    }
    discovered
        .lock()
        .ok()?
        .as_ref()
        .filter(|d| d.last_seen.elapsed() < discovery::BEACON_TIMEOUT)
        .map(|d| d.ip.clone())
}

// ── Engine loop ───────────────────────────────────────────────────────────────

fn run(
    state: Arc<Mutex<AppState>>,
    cmd_rx: Receiver<EngineCmd>,
    preview_tx: SyncSender<Vec<u8>>,
    initial_config: Config,
    ffmpeg_pid: Arc<Mutex<Option<u32>>>,
    discovered: Arc<Mutex<Option<discovery::DiscoveredDevice>>>,
) {
    let mut config = initial_config;
    let mut ffmpeg_proc: Option<Child> = None;
    let mut active = false;
    let mut device_ready = false;
    // Resolved IP used when FFmpeg is spawned in WiFi mode.
    let mut wifi_target_ip: Option<String> = None;

    // Force an immediate check on first Start command
    let mut last_check = Instant::now() - Duration::from_secs(60);

    loop {
        // ── Process all pending commands (non-blocking) ───────────────────────
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                EngineCmd::Start => {
                    eprintln!("[engine] Start → WaitingDevice");
                    active = true;
                    last_check = Instant::now() - Duration::from_secs(60);
                    set_status(&state, Status::WaitingDevice);
                }
                EngineCmd::Stop => {
                    eprintln!("[engine] Stop → Idle");
                    active = false;
                    ffmpeg::kill(&mut ffmpeg_proc);
                    store_pid(&ffmpeg_pid, None);
                    if device_ready && config.connection_mode == ConnectionMode::Usb {
                        adb::remove_forward(config.adb_port);
                    }
                    device_ready = false;
                    wifi_target_ip = None;
                    set_status(&state, Status::Idle);
                    set_discovered_ip(&state, None);
                }
                EngineCmd::UpdateConfig(new_cfg) => {
                    let mode_changed = new_cfg.connection_mode != config.connection_mode;
                    let port_changed = new_cfg.adb_port != config.adb_port;
                    let old_port = config.adb_port;
                    config = new_cfg;

                    if ffmpeg_proc.is_some() {
                        ffmpeg::kill(&mut ffmpeg_proc);
                        store_pid(&ffmpeg_pid, None);
                        set_status(&state, Status::Connecting);
                    }
                    if mode_changed || port_changed {
                        // Clean up any active ADB forward from the old mode/port.
                        if mode_changed || config.connection_mode == ConnectionMode::Usb {
                            adb::remove_forward(old_port);
                        }
                        device_ready = false;
                        wifi_target_ip = None;
                        last_check = Instant::now() - Duration::from_secs(60);
                    }
                }
            }
        }

        if active {
            // ── Periodic device check (every 2 s) ────────────────────────────
            if last_check.elapsed() >= Duration::from_secs(2) {
                last_check = Instant::now();

                match config.connection_mode {
                    // ── USB: poll ADB ─────────────────────────────────────────
                    ConnectionMode::Usb => {
                        let connected = adb::device_connected();

                        if connected && !device_ready {
                            eprintln!("[engine] USB device detected, setting up ADB forward :{}", config.adb_port);
                            if adb::forward(config.adb_port) {
                                eprintln!("[engine] ADB forward ok → Connecting");
                                device_ready = true;
                            } else {
                                eprintln!("[engine] ADB forward failed → Error");
                                set_status(&state, Status::Error("ADB forward falló".to_string()));
                            }
                        } else if !connected && device_ready {
                            eprintln!("[engine] USB device lost → WaitingDevice");
                            ffmpeg::kill(&mut ffmpeg_proc);
                            store_pid(&ffmpeg_pid, None);
                            adb::remove_forward(config.adb_port);
                            device_ready = false;
                            set_status(&state, Status::WaitingDevice);
                        } else if !connected {
                            set_status(&state, Status::WaitingDevice);
                        }
                    }

                    // ── WiFi: watch beacon / manual IP ────────────────────────
                    ConnectionMode::Wifi => {
                        let target = resolve_wifi_ip(&config, &discovered);

                        // Expose the resolved IP to the GUI (None = not found yet).
                        set_discovered_ip(&state, target.clone());

                        match target {
                            Some(ip) if !device_ready => {
                                eprintln!("[engine] WiFi device ready at {ip} → Connecting");
                                wifi_target_ip = Some(ip);
                                device_ready = true;
                            }
                            None if device_ready => {
                                // Beacon timed out and no manual IP — stop streaming.
                                eprintln!("[engine] WiFi beacon lost → WaitingDevice");
                                ffmpeg::kill(&mut ffmpeg_proc);
                                store_pid(&ffmpeg_pid, None);
                                wifi_target_ip = None;
                                device_ready = false;
                                set_status(&state, Status::WaitingDevice);
                            }
                            None => {
                                set_status(&state, Status::WaitingDevice);
                            }
                            _ => {} // Some(ip) while already device_ready → no-op
                        }
                    }
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
                        eprintln!("[engine] FFmpeg exited, respawning in 500 ms…");
                        ffmpeg::kill(&mut ffmpeg_proc);
                        store_pid(&ffmpeg_pid, None);
                        set_status(&state, Status::Connecting);
                        thread::sleep(Duration::from_millis(500));
                    }

                    set_status(&state, Status::Connecting);

                    let (host, port) = match config.connection_mode {
                        ConnectionMode::Usb => ("localhost".to_string(), config.adb_port),
                        ConnectionMode::Wifi => (
                            wifi_target_ip.clone().unwrap_or_default(),
                            config.adb_port,
                        ),
                    };

                    eprintln!("[engine] Spawning FFmpeg → tcp://{}:{}", host, port);
                    match ffmpeg::spawn_ffmpeg(&config, &host, port, preview_tx.clone()) {
                        Some((proc, pid)) => {
                            eprintln!("[engine] FFmpeg spawned (pid={pid}) → Streaming");
                            store_pid(&ffmpeg_pid, Some(pid));
                            ffmpeg_proc = Some(proc);
                            set_status(&state, Status::Streaming);
                        }
                        None => {
                            eprintln!("[engine] FFmpeg spawn failed → Error (retry in 2 s)");
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
