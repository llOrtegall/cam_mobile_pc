#!/usr/bin/env bash
# launch.sh — Prepara el entorno y lanza campc.py
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "[launch] Verificando v4l2loopback..."
if lsmod | grep -q v4l2loopback; then
    echo "[launch] Recargando módulo para limpiar estado..."
    sudo rmmod v4l2loopback
fi

sudo modprobe v4l2loopback video_nr=10 card_label="AndroidCam" exclusive_caps=1 || {
    echo "[launch] ERROR: no se pudo cargar v4l2loopback."
    echo "         Ejecuta primero: bash setup_ubuntu.sh"
    exit 1
}
echo "[launch] /dev/video10 listo."

echo "[launch] Iniciando campc.py..."
python3 "$SCRIPT_DIR/campc.py"
