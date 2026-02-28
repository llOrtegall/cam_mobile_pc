#!/usr/bin/env bash
# launch.sh — Prepara el entorno y lanza campc.py
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "[launch] Verificando v4l2loopback..."
if ! lsmod | grep -q v4l2loopback; then
    echo "[launch] Cargando módulo v4l2loopback..."
    sudo modprobe v4l2loopback video_nr=10 card_label="AndroidCam" exclusive_caps=1 || {
        echo "[launch] ERROR: no se pudo cargar v4l2loopback."
        echo "         Ejecuta primero: bash setup_ubuntu.sh"
        exit 1
    }
fi

echo "[launch] /dev/video10 listo."

# Liberar el dispositivo si está ocupado por un proceso anterior
if fuser /dev/video10 &>/dev/null; then
    echo "[launch] Liberando /dev/video10 ocupado..."
    sudo fuser -k /dev/video10
    sleep 1
fi

echo "[launch] Iniciando campc.py..."
python3 "$SCRIPT_DIR/campc.py"
