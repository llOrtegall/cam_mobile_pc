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

/// Runs `adb forward tcp:PORT tcp:PORT`. Returns true on success.
pub fn forward(port: u16) -> bool {
    let spec = format!("tcp:{port}");
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
