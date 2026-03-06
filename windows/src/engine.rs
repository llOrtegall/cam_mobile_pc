use std::process::Child;
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use log::{error, info, warn};

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

const FORCE_CHECK_WINDOW: Duration = Duration::from_secs(60);
const DEVICE_CHECK_INTERVAL: Duration = Duration::from_secs(2);
const LOOP_TICK: Duration = Duration::from_millis(200);
const RESPAWN_DELAY: Duration = Duration::from_millis(500);
const SPAWN_FAIL_DELAY: Duration = Duration::from_secs(2);

struct StreamState {
    ffmpeg_proc: Option<Child>,
    device_ready: bool,
    wifi_target_ip: Option<String>,
    last_check: Instant,
}

fn force_immediate_check(last_check: &mut Instant) {
    *last_check = Instant::now() - FORCE_CHECK_WINDOW;
}

fn clear_ffmpeg(ffmpeg_proc: &mut Option<Child>, ffmpeg_pid: &Arc<Mutex<Option<u32>>>) {
    ffmpeg::kill(ffmpeg_proc);
    store_pid(ffmpeg_pid, None);
}

fn on_start(
    state: &Arc<Mutex<AppState>>,
    active: &mut bool,
    stream: &mut StreamState,
) {
    info!("[engine] Start → WaitingDevice");
    *active = true;
    force_immediate_check(&mut stream.last_check);
    set_status(state, Status::WaitingDevice);
}

fn on_stop(
    state: &Arc<Mutex<AppState>>,
    config: &Config,
    ffmpeg_pid: &Arc<Mutex<Option<u32>>>,
    active: &mut bool,
    stream: &mut StreamState,
) {
    info!("[engine] Stop → Idle");
    *active = false;
    clear_ffmpeg(&mut stream.ffmpeg_proc, ffmpeg_pid);

    if stream.device_ready && config.connection_mode == ConnectionMode::Usb {
        adb::remove_forward(config.adb_port);
    }

    stream.device_ready = false;
    stream.wifi_target_ip = None;
    set_status(state, Status::Idle);
    set_discovered_ip(state, None);
}

fn on_update_config(
    state: &Arc<Mutex<AppState>>,
    config: &mut Config,
    new_cfg: Config,
    ffmpeg_pid: &Arc<Mutex<Option<u32>>>,
    stream: &mut StreamState,
) {
    let mode_changed = new_cfg.connection_mode != config.connection_mode;
    let port_changed = new_cfg.adb_port != config.adb_port;
    let old_port = config.adb_port;
    *config = new_cfg;

    if stream.ffmpeg_proc.is_some() {
        clear_ffmpeg(&mut stream.ffmpeg_proc, ffmpeg_pid);
        set_status(state, Status::Connecting);
    }
    if mode_changed || port_changed {
        // Old forward must be removed before reconnect to avoid stale tunnels.
        if mode_changed || config.connection_mode == ConnectionMode::Usb {
            adb::remove_forward(old_port);
        }
        stream.device_ready = false;
        stream.wifi_target_ip = None;
        force_immediate_check(&mut stream.last_check);
    }
}

fn poll_usb(
    state: &Arc<Mutex<AppState>>,
    config: &Config,
    ffmpeg_pid: &Arc<Mutex<Option<u32>>>,
    stream: &mut StreamState,
) {
    let connected = adb::device_connected();

    if connected && !stream.device_ready {
        info!("[engine] USB device detected, setting up ADB forward :{}", config.adb_port);
        if adb::forward(config.adb_port) {
            info!("[engine] ADB forward ok → Connecting");
            stream.device_ready = true;
        } else {
            error!("[engine] ADB forward failed → Error");
            set_status(state, Status::Error("ADB forward falló".to_string()));
        }
    } else if !connected && stream.device_ready {
        warn!("[engine] USB device lost → WaitingDevice");
        clear_ffmpeg(&mut stream.ffmpeg_proc, ffmpeg_pid);
        adb::remove_forward(config.adb_port);
        stream.device_ready = false;
        set_status(state, Status::WaitingDevice);
    } else if !connected {
        set_status(state, Status::WaitingDevice);
    }
}

fn poll_wifi(
    state: &Arc<Mutex<AppState>>,
    config: &Config,
    discovered: &Arc<Mutex<Option<discovery::DiscoveredDevice>>>,
    ffmpeg_pid: &Arc<Mutex<Option<u32>>>,
    stream: &mut StreamState,
) {
    let target = resolve_wifi_ip(config, discovered);

    // Publish resolved IP to GUI (`None` means no active beacon/manual target).
    set_discovered_ip(state, target.clone());

    match target {
        Some(ip) if !stream.device_ready => {
            info!("[engine] WiFi device ready at {ip} → Connecting");
            stream.wifi_target_ip = Some(ip);
            stream.device_ready = true;
        }
        None if stream.device_ready => {
            warn!("[engine] WiFi beacon lost → WaitingDevice");
            clear_ffmpeg(&mut stream.ffmpeg_proc, ffmpeg_pid);
            stream.wifi_target_ip = None;
            stream.device_ready = false;
            set_status(state, Status::WaitingDevice);
        }
        None => {
            set_status(state, Status::WaitingDevice);
        }
        _ => {}
    }
}

fn try_spawn_ffmpeg(
    state: &Arc<Mutex<AppState>>,
    preview_tx: &SyncSender<Vec<u8>>,
    config: &Config,
    ffmpeg_pid: &Arc<Mutex<Option<u32>>>,
    stream: &mut StreamState,
) {
    let ffmpeg_exited = stream
        .ffmpeg_proc
        .as_mut()
        .map(|p| p.try_wait().ok().flatten().is_some())
        .unwrap_or(true);

    if !ffmpeg_exited {
        return;
    }

    if stream.ffmpeg_proc.is_some() {
        warn!("[engine] FFmpeg exited, respawning in 500 ms…");
        clear_ffmpeg(&mut stream.ffmpeg_proc, ffmpeg_pid);
        set_status(state, Status::Connecting);
        thread::sleep(RESPAWN_DELAY);
    }

    set_status(state, Status::Connecting);

    let (host, port) = match config.connection_mode {
        ConnectionMode::Usb => ("localhost".to_string(), config.adb_port),
        ConnectionMode::Wifi => (stream.wifi_target_ip.clone().unwrap_or_default(), config.adb_port),
    };

    info!("[engine] Spawning FFmpeg → tcp://{}:{}", host, port);
    match ffmpeg::spawn_ffmpeg(config, &host, port, preview_tx.clone()) {
        Some((proc, pid)) => {
            info!("[engine] FFmpeg spawned (pid={pid}) → Streaming");
            store_pid(ffmpeg_pid, Some(pid));
            stream.ffmpeg_proc = Some(proc);
            set_status(state, Status::Streaming);
        }
        None => {
            error!("[engine] FFmpeg spawn failed → Error (retry in 2 s)");
            store_pid(ffmpeg_pid, None);
            set_status(state, Status::Error("No se pudo iniciar FFmpeg".to_string()));
            thread::sleep(SPAWN_FAIL_DELAY);
        }
    }
}

fn run(
    state: Arc<Mutex<AppState>>,
    cmd_rx: Receiver<EngineCmd>,
    preview_tx: SyncSender<Vec<u8>>,
    initial_config: Config,
    ffmpeg_pid: Arc<Mutex<Option<u32>>>,
    discovered: Arc<Mutex<Option<discovery::DiscoveredDevice>>>,
) {
    let mut config = initial_config;
    let mut active = false;
    let mut stream = StreamState {
        ffmpeg_proc: None,
        device_ready: false,
        wifi_target_ip: None,
        last_check: Instant::now() - FORCE_CHECK_WINDOW,
    };

    loop {
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                EngineCmd::Start => on_start(&state, &mut active, &mut stream),
                EngineCmd::Stop => on_stop(
                    &state,
                    &config,
                    &ffmpeg_pid,
                    &mut active,
                    &mut stream,
                ),
                EngineCmd::UpdateConfig(new_cfg) => on_update_config(
                    &state,
                    &mut config,
                    new_cfg,
                    &ffmpeg_pid,
                    &mut stream,
                ),
            }
        }

        if active {
            if stream.last_check.elapsed() >= DEVICE_CHECK_INTERVAL {
                stream.last_check = Instant::now();
                match config.connection_mode {
                    ConnectionMode::Usb => poll_usb(
                        &state,
                        &config,
                        &ffmpeg_pid,
                        &mut stream,
                    ),
                    ConnectionMode::Wifi => poll_wifi(
                        &state,
                        &config,
                        &discovered,
                        &ffmpeg_pid,
                        &mut stream,
                    ),
                }
            }

            if stream.device_ready {
                try_spawn_ffmpeg(
                    &state,
                    &preview_tx,
                    &config,
                    &ffmpeg_pid,
                    &mut stream,
                );
            }
        }

        thread::sleep(LOOP_TICK);
    }
}
