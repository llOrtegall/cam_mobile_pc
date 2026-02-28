#!/usr/bin/env bash
# setup_ubuntu.sh — One-time setup for cam-mobile-pc on Ubuntu
# Run once per machine (or after kernel upgrades for DKMS rebuild)
set -e

echo "=== cam-mobile-pc Ubuntu Setup ==="

# ---- System packages ----
sudo apt update
sudo apt install -y \
    android-tools-adb \
    v4l2loopback-dkms \
    v4l-utils \
    python3-pip \
    python3-tk

# ---- Python packages ----
pip3 install --user opencv-python pyfakewebcam Pillow

# ---- Load v4l2loopback now ----
if lsmod | grep -q v4l2loopback; then
    echo "v4l2loopback already loaded — reloading with correct options..."
    sudo modprobe -r v4l2loopback
fi
sudo modprobe v4l2loopback video_nr=10 card_label="AndroidCam" exclusive_caps=1
echo "Loaded v4l2loopback → /dev/video10"

# ---- Persist across reboots ----
echo "v4l2loopback" | sudo tee /etc/modules-load.d/v4l2loopback.conf > /dev/null

sudo tee /etc/modprobe.d/v4l2loopback.conf > /dev/null <<'EOF'
options v4l2loopback devices=1 video_nr=10 card_label="AndroidCam" exclusive_caps=1
EOF

echo ""
echo "=== Setup complete ==="
echo "Verify with:"
echo "  lsmod | grep v4l2loopback"
echo "  v4l2-ctl --list-devices"
echo ""
echo "Then run:  python3 linux/campc.py"
