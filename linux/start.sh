#!/usr/bin/env bash
# start.sh — Forward ADB port and pipe MJPEG stream into the v4l2loopback virtual camera.
# Keep this running while using the phone camera as a webcam.

# Re-exec with bash if invoked via `sh start.sh`
[ -z "$BASH_VERSION" ] && exec bash "$0" "$@"

set -euo pipefail

ADB_PORT=5000
V4L2_DEVICE=/dev/video10

# ---- Auto-install missing deps ----------------------------------------------
if ! command -v adb &>/dev/null || ! command -v ffmpeg &>/dev/null; then
    echo "==> Installing missing dependencies (adb, ffmpeg)..."
    sudo dnf install -y android-tools ffmpeg
fi

if [[ ! -e "$V4L2_DEVICE" ]]; then
    echo "==> $V4L2_DEVICE not found — loading v4l2loopback..."

    # Ensure package is installed
    if ! rpm -q akmod-v4l2loopback &>/dev/null; then
        echo "==> Installing akmod-v4l2loopback via RPM Fusion..."
        sudo dnf install -y \
            "https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm" \
            || true
        sudo dnf install -y akmod-v4l2loopback
        echo "==> Building kernel module (may take a few minutes)..."
        sudo akmods --force --kernels "$(uname -r)"
        sudo depmod -a
    fi

    # The v4l2loopback RPM ships /usr/lib/modprobe.d/v4l2loopback.conf with OBS defaults
    # that override /etc/modprobe.d/ options. Use the 'install' directive instead —
    # it intercepts ANY modprobe call for the module (including from systemd-modules-load)
    # and forces our exact parameters.
    sudo tee /etc/modprobe.d/v4l2loopback.conf > /dev/null <<'EOF'
install v4l2loopback /sbin/modprobe --ignore-install v4l2loopback devices=1 video_nr=10 card_label=AndroidCam exclusive_caps=1
EOF

    # Unload any currently-loaded instance (may be at wrong video_nr)
    if lsmod | grep -q v4l2loopback; then
        echo "==> Unloading existing v4l2loopback instance..."
        sudo modprobe -r v4l2loopback || sudo rmmod v4l2loopback || true
        sleep 1
    fi

    echo "==> Loading v4l2loopback with our parameters..."
    sudo modprobe v4l2loopback
    sudo udevadm settle

    if [[ ! -e "$V4L2_DEVICE" ]]; then
        echo "ERROR: $V4L2_DEVICE still not found." >&2
        echo "       Devices: $(ls /dev/video* 2>/dev/null || echo 'none')" >&2
        echo "       Names:   $(cat /sys/devices/virtual/video4linux/*/name 2>/dev/null || echo 'n/a')" >&2
        echo "       Module:  $(lsmod | grep v4l2loopback || echo 'not loaded')" >&2
        exit 1
    fi
    echo "==> $V4L2_DEVICE ready (AndroidCam)."
fi

echo "==> Waiting for Android device..."
adb wait-for-device
echo "==> Device ready: $(adb devices | grep -v '^List' | head -1)"

trap 'echo ""; echo "==> Cleaning up ADB forward..."; adb forward --remove tcp:${ADB_PORT} 2>/dev/null || true' EXIT

echo "==> Starting stream loop. Press Ctrl+C to stop."
echo "    Phone app: open CamPC → Start Streaming"
echo "    PC camera: $V4L2_DEVICE"
echo ""

while true; do
    # Re-establish port forward each iteration (survives USB reconnect)
    if ! adb forward tcp:${ADB_PORT} tcp:${ADB_PORT}; then
        echo "[$(date +%T)] ADB forward failed, retrying in 3s..."
        sleep 3
        continue
    fi

    echo "[$(date +%T)] ADB forward active. Connecting to stream..."

    ffmpeg \
        -loglevel warning \
        -probesize 32 \
        -analyzeduration 0 \
        -f mpjpeg \
        -reconnect 1 \
        -reconnect_at_eof 1 \
        -reconnect_streamed 1 \
        -reconnect_delay_max 5 \
        -i "tcp://localhost:${ADB_PORT}" \
        -vf "scale=1280:720,format=yuyv422" \
        -f v4l2 \
        "$V4L2_DEVICE" \
        || true  # don't exit on ffmpeg error — loop and retry

    adb forward --remove tcp:${ADB_PORT} 2>/dev/null || true
    echo "[$(date +%T)] Stream ended, retrying in 2s..."
    sleep 2
done
