use std::net::UdpSocket;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub const BEACON_PORT: u16 = 5001;
pub const BEACON_TIMEOUT: Duration = Duration::from_secs(5);

pub struct DiscoveredDevice {
    pub ip: String,
    pub last_seen: Instant,
}

/// Spawns a background UDP listener on port 5001.
///
/// Returns a shared handle updated whenever a "CAMPC_HELLO" beacon arrives.
/// The thread runs for the lifetime of the process.
pub fn start_listener() -> Arc<Mutex<Option<DiscoveredDevice>>> {
    let state: Arc<Mutex<Option<DiscoveredDevice>>> = Arc::new(Mutex::new(None));
    let state_clone = Arc::clone(&state);

    thread::spawn(move || {
        let socket = match UdpSocket::bind(("0.0.0.0", BEACON_PORT)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[discovery] Cannot bind UDP :{BEACON_PORT}: {e}");
                return;
            }
        };
        // Use a short timeout so the thread is responsive to process exit.
        let _ = socket.set_read_timeout(Some(Duration::from_secs(1)));

        let mut buf = [0u8; 64];
        loop {
            match socket.recv_from(&mut buf) {
                Ok((n, src)) if &buf[..n] == b"CAMPC_HELLO" => {
                    let ip = src.ip().to_string();
                    if let Ok(mut guard) = state_clone.lock() {
                        *guard = Some(DiscoveredDevice {
                            ip,
                            last_seen: Instant::now(),
                        });
                    }
                }
                _ => {} // timeout or unrecognised packet — keep looping
            }
        }
    });

    state
}
