#!/usr/bin/env bash
# setup_ubuntu.sh — One-time setup for cam-mobile-pc on Ubuntu
# Run once per machine (or after kernel upgrades for DKMS rebuild)
set -e

echo "=== cam-mobile-pc Ubuntu Setup ==="

# ── System packages ────────────────────────────────────────────────────────────
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

# ── v4l2loopback (build from upstream source) ──────────────────────────────────
# The Ubuntu apt package (0.12.7) does NOT set VIDEO_CAPTURE/VIDEO_OUTPUT bits
# on kernel 6.x, causing VIDIOC_S_FMT to always return EINVAL.  Build from the
# latest upstream source which has the kernel 6.x fixes.
echo ""
echo "Building v4l2loopback from upstream source…"

# Remove apt package if present (avoids conflicts)
if dpkg -l v4l2loopback-dkms &>/dev/null 2>&1; then
    echo "Removing apt v4l2loopback-dkms (incompatible with kernel $(uname -r))…"
    sudo apt remove -y v4l2loopback-dkms
fi

V4L2_SRC="/tmp/v4l2loopback-src"
rm -rf "$V4L2_SRC"
git clone --depth=1 https://github.com/umlaeute/v4l2loopback.git "$V4L2_SRC"
cd "$V4L2_SRC"
make
sudo make install
sudo depmod -a
echo "v4l2loopback installed from upstream source."
cd -

# ── Rust toolchain ─────────────────────────────────────────────────────────────
if ! command -v cargo &>/dev/null && ! [ -x "$HOME/.cargo/bin/cargo" ]; then
    echo "Installing Rust toolchain via rustup..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
    # shellcheck disable=SC1090
    source "$HOME/.cargo/env"
else
    echo "Rust already installed: $(rustc --version 2>/dev/null || $HOME/.cargo/bin/rustc --version)"
fi

# ── Build campc (Rust + egui) ──────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
echo ""
echo "Building campc binary (release)…"
"${HOME}/.cargo/bin/cargo" build --release --manifest-path "$SCRIPT_DIR/Cargo.toml"
BIN="$SCRIPT_DIR/target/release/campc"
echo "Binary: $BIN"

# ── Load v4l2loopback now ──────────────────────────────────────────────────────
if lsmod | grep -q v4l2loopback; then
    echo "v4l2loopback already loaded — reloading with correct options..."
    sudo modprobe -r v4l2loopback
fi
sudo modprobe v4l2loopback video_nr=10 card_label="AndroidCam" exclusive_caps=1
echo "Loaded v4l2loopback → /dev/video10"

# Verify caps include VIDEO_CAPTURE (0x1) — required by Zoom/Meet/Teams
CAPS=$(python3 -c "
import fcntl, struct, os, ctypes
VIDIOC_QUERYCAP = 0x80685600
fd = os.open('/dev/video10', os.O_RDONLY)
buf = bytearray(104)
fcntl.ioctl(fd, VIDIOC_QUERYCAP, buf)
caps = struct.unpack_from('<I', buf, 20)[0]
print(f'0x{caps:08X}')
os.close(fd)
" 2>/dev/null || echo "unknown")
echo "Device caps: $CAPS"
if [[ "$CAPS" == *"unknown"* ]]; then
    echo "WARNING: could not query device caps"
elif python3 -c "import sys; sys.exit(0 if int('$CAPS',16) & 0x1 else 1)" 2>/dev/null; then
    echo "OK: VIDEO_CAPTURE bit is set — Zoom/Meet/Teams will see the device."
else
    echo "WARNING: VIDEO_CAPTURE bit NOT set (caps=$CAPS)."
    echo "  The upstream v4l2loopback build may not have installed correctly."
    echo "  Check: sudo dmesg | grep v4l2loopback"
fi

# ── Persist module across reboots ─────────────────────────────────────────────
echo "v4l2loopback" | sudo tee /etc/modules-load.d/v4l2loopback.conf > /dev/null

sudo tee /etc/modprobe.d/v4l2loopback.conf > /dev/null <<'EOF'
options v4l2loopback devices=1 video_nr=10 card_label="AndroidCam" exclusive_caps=1
EOF

echo ""
echo "=== Setup complete ==="
echo "Verify with:"
echo "  lsmod | grep v4l2loopback"
echo "  v4l2-ctl --list-devices"
echo "  cat /sys/module/v4l2loopback/parameters/exclusive_caps"
echo ""
echo "Run:  $BIN"
