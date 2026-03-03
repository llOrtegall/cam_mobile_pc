use std::process::Command;

/// Returns true if at least one ADB device is connected and authorised.
pub fn device_connected() -> bool {
    let Ok(output) = Command::new("adb").args(["devices"]).output() else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .skip(1) // skip "List of devices attached"
        .any(|line| line.ends_with("device")) // excludes "offline" / "unauthorized"
}

/// Establishes the ADB port forward for `port`.
///
/// Removes any existing (potentially stale) forward first so ADB starts
/// from a clean state. Returns true if the forward is active afterwards.
pub fn forward(port: u16) -> bool {
    let spec = format!("tcp:{port}");
    // Remove first to clear any stale connection state from a previous session.
    let _ = Command::new("adb")
        .args(["forward", "--remove", &spec])
        .status();
    Command::new("adb")
        .args(["forward", &spec, &spec])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Removes the ADB forward rule (best-effort; ignores errors).
pub fn remove_forward(port: u16) {
    let spec = format!("tcp:{port}");
    let _ = Command::new("adb")
        .args(["forward", "--remove", &spec])
        .status();
}