#!/usr/bin/env bash
# setup.sh — Install dependencies and configure v4l2loopback virtual webcam
# Run once on the Linux PC (Fedora). Re-run after kernel upgrades.

# Re-exec with bash if invoked via `sh setup.sh`
[ -z "$BASH_VERSION" ] && exec bash "$0" "$@"

set -euo pipefail

DEVICE_NR=10
CARD_LABEL="AndroidCam"

echo "==> Installing ADB and v4l2-utils..."
sudo dnf install -y android-tools v4l-utils ffmpeg

# ---- v4l2loopback via RPM Fusion (preferred) --------------------------------
install_via_rpmfusion() {
    echo "==> Enabling RPM Fusion free repo..."
    sudo dnf install -y \
        "https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm" \
        || true  # ignore if already enabled

    echo "==> Installing akmod-v4l2loopback..."
    sudo dnf install -y akmod-v4l2loopback

    echo "==> Building kernel module (this may take a few minutes)..."
    sudo akmods --force --kernels "$(uname -r)"
}

# ---- v4l2loopback from source (fallback) ------------------------------------
install_from_source() {
    echo "==> Building v4l2loopback from source..."
    local build_dir
    build_dir=$(mktemp -d)
    git clone --depth 1 https://github.com/umlaeute/v4l2loopback.git "$build_dir/v4l2loopback"
    pushd "$build_dir/v4l2loopback"
    make
    sudo make install
    sudo depmod -a
    popd
    rm -rf "$build_dir"
}

# Try RPM Fusion first; fall back to building from source
if install_via_rpmfusion; then
    echo "==> akmod-v4l2loopback installed via RPM Fusion."
else
    echo "==> RPM Fusion install failed, building from source..."
    install_from_source
fi

# ---- Load module now --------------------------------------------------------
echo "==> Loading v4l2loopback module..."
sudo modprobe v4l2loopback \
    devices=1 \
    video_nr=${DEVICE_NR} \
    card_label="${CARD_LABEL}" \
    exclusive_caps=1

# ---- Persist across reboots -------------------------------------------------
echo "==> Configuring module to load at boot..."

echo "v4l2loopback" | sudo tee /etc/modules-load.d/v4l2loopback.conf

sudo tee /etc/modprobe.d/v4l2loopback.conf > /dev/null <<EOF
options v4l2loopback devices=1 video_nr=${DEVICE_NR} card_label="${CARD_LABEL}" exclusive_caps=1
EOF

# ---- Verify -----------------------------------------------------------------
echo ""
echo "==> Verifying /dev/video${DEVICE_NR}..."
v4l2-ctl --device=/dev/video${DEVICE_NR} --info

echo ""
echo "==> Setup complete!"
echo "    Virtual webcam: /dev/video${DEVICE_NR} (\"${CARD_LABEL}\")"
echo "    Next step: plug in phone and run  linux/start.sh"
