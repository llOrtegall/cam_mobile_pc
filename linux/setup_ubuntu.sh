#!/usr/bin/env bash
# setup_ubuntu.sh — One-time setup for cam-mobile-pc on Ubuntu
# Run once per machine (or after kernel upgrades for module rebuild)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
V4L2_SRC="/tmp/v4l2loopback-src"
CARGO_BIN="${HOME}/.cargo/bin/cargo"
RUSTC_BIN="${HOME}/.cargo/bin/rustc"
BIN_PATH="${SCRIPT_DIR}/target/release/campc"

log() {
    printf '%s\n' "$*"
}

warn() {
    printf 'WARNING: %s\n' "$*" >&2
}

require_cmd() {
    command -v "$1" >/dev/null 2>&1 || {
        warn "Required command not found: $1"
        exit 1
    }
}

install_system_packages() {
    sudo apt update
    sudo apt install -y \
        android-tools-adb \
        v4l-utils \
        ffmpeg \
        git \
        dkms \
        build-essential \
        "linux-headers-$(uname -r)" \
        libxcb-render0-dev \
        libxcb-shape0-dev \
        libxcb-xfixes0-dev \
        libxkbcommon-dev
}

remove_apt_v4l2loopback_if_present() {
    if dpkg -s v4l2loopback-dkms >/dev/null 2>&1; then
        log "Removing apt v4l2loopback-dkms (incompatible with kernel $(uname -r))…"
        sudo apt remove -y v4l2loopback-dkms
    fi
}

build_upstream_v4l2loopback() {
    log ""
    log "Building v4l2loopback from upstream source…"

    remove_apt_v4l2loopback_if_present

    rm -rf "${V4L2_SRC}"
    git clone --depth=1 https://github.com/umlaeute/v4l2loopback.git "${V4L2_SRC}"

    pushd "${V4L2_SRC}" >/dev/null
    make
    sudo make install
    sudo depmod -a
    popd >/dev/null

    log "v4l2loopback installed from upstream source."
}

ensure_rust_toolchain() {
    if ! command -v cargo >/dev/null 2>&1 && [[ ! -x "${CARGO_BIN}" ]]; then
        log "Installing Rust toolchain via rustup..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
        # shellcheck disable=SC1090
        source "${HOME}/.cargo/env"
    else
        local rustc_version
        rustc_version="$(rustc --version 2>/dev/null || "${RUSTC_BIN}" --version 2>/dev/null || echo "unknown")"
        log "Rust already installed: ${rustc_version}"
    fi
}

build_campc() {
    log ""
    log "Building campc binary (release)…"
    "${CARGO_BIN}" build --release --manifest-path "${SCRIPT_DIR}/Cargo.toml"
    log "Binary: ${BIN_PATH}"
}

load_v4l2loopback_module() {
    if lsmod | grep -q '^v4l2loopback'; then
        log "v4l2loopback already loaded — reloading with correct options..."
        sudo modprobe -r v4l2loopback
    fi

    sudo modprobe v4l2loopback video_nr=10 card_label="AndroidCam" exclusive_caps=1
    log "Loaded v4l2loopback → /dev/video10"
}

query_device_caps() {
    python3 -c "
import fcntl, os, struct
VIDIOC_QUERYCAP = 0x80685600
fd = os.open('/dev/video10', os.O_RDONLY)
buf = bytearray(104)
fcntl.ioctl(fd, VIDIOC_QUERYCAP, buf)
caps = struct.unpack_from('<I', buf, 20)[0]
print(f'0x{caps:08X}')
os.close(fd)
" 2>/dev/null || echo "unknown"
}

verify_capture_capability() {
    local caps
    caps="$(query_device_caps)"
    log "Device caps: ${caps}"

    if [[ "${caps}" == "unknown" ]]; then
        warn "could not query device caps"
    elif python3 -c "import sys; sys.exit(0 if int('${caps}',16) & 0x1 else 1)" 2>/dev/null; then
        log "OK: VIDEO_CAPTURE bit is set — Zoom/Meet/Teams will see the device."
    else
        warn "VIDEO_CAPTURE bit NOT set (caps=${caps})."
        log "  The upstream v4l2loopback build may not have installed correctly."
        log "  Check: sudo dmesg | grep v4l2loopback"
    fi
}

persist_module_config() {
    echo "v4l2loopback" | sudo tee /etc/modules-load.d/v4l2loopback.conf >/dev/null

    sudo tee /etc/modprobe.d/v4l2loopback.conf >/dev/null <<'CONFIG_EOF'
options v4l2loopback devices=1 video_nr=10 card_label="AndroidCam" exclusive_caps=1
CONFIG_EOF
}

print_summary() {
    log ""
    log "=== Setup complete ==="
    log "Verify with:"
    log "  lsmod | grep v4l2loopback"
    log "  v4l2-ctl --list-devices"
    log "  cat /sys/module/v4l2loopback/parameters/exclusive_caps"
    log ""
    log "Run:  ${BIN_PATH}"
}

main() {
    log "=== cam-mobile-pc Ubuntu Setup ==="

    require_cmd sudo
    require_cmd apt
    require_cmd git
    require_cmd make
    require_cmd python3

    install_system_packages
    build_upstream_v4l2loopback
    ensure_rust_toolchain
    build_campc
    load_v4l2loopback_module
    verify_capture_capability
    persist_module_config
    print_summary
}

main "$@"
