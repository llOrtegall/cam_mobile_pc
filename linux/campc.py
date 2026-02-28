#!/usr/bin/env python3
"""
campc.py — CamPC Linux GUI
Streams Android rear camera to /dev/video10 (v4l2loopback) via ADB/TCP.
Requires: opencv-python, pyfakewebcam, Pillow, python3-tk
"""

import atexit
import signal
import subprocess
import threading
import time
import tkinter as tk
from tkinter import ttk

import cv2
import pyfakewebcam
from PIL import Image, ImageTk

# ── Constants ─────────────────────────────────────────────────────────────────

TCP_URL = "tcp://localhost:5000"
V4L2_DEVICE = "/dev/video10"
PREVIEW_W, PREVIEW_H = 640, 360

RESOLUTIONS = {
    "720p": (1280, 720),
    "1080p": (1920, 1080),
    "480p": (854, 480),
}

ROTATE_MAP = {
    0: None,
    90: cv2.ROTATE_90_CLOCKWISE,
    180: cv2.ROTATE_180,
    270: cv2.ROTATE_90_COUNTERCLOCKWISE,
}


# ── Transforms ────────────────────────────────────────────────────────────────

def apply_zoom(frame, zoom: float):
    """Centre-crop then scale back to original size."""
    if zoom <= 1.0:
        return frame
    h, w = frame.shape[:2]
    ch, cw = int(h / zoom), int(w / zoom)
    y1, x1 = (h - ch) // 2, (w - cw) // 2
    return cv2.resize(frame[y1:y1 + ch, x1:x1 + cw], (w, h),
                      interpolation=cv2.INTER_LINEAR)


def apply_rotation(frame, deg: int):
    code = ROTATE_MAP.get(deg)
    return cv2.rotate(frame, code) if code is not None else frame


# ── ADB helpers ───────────────────────────────────────────────────────────────

def adb_forward():
    """Establish ADB TCP forward; returns True on success."""
    try:
        result = subprocess.run(
            ["adb", "forward", "tcp:5000", "tcp:5000"],
            capture_output=True, timeout=5
        )
        return result.returncode == 0
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return False


def adb_remove_forward():
    try:
        subprocess.run(["adb", "forward", "--remove", "tcp:5000"],
                       capture_output=True, timeout=5)
    except Exception:
        pass


# ── App ───────────────────────────────────────────────────────────────────────

class CamPCApp:
    def __init__(self, root: tk.Tk):
        self.root = root
        self.root.title("CamPC")
        self.root.resizable(False, False)

        # ── Shared state ──────────────────────────────────────────────────────
        self._running = False
        self._capture_thread: threading.Thread | None = None
        self._lock = threading.Lock()
        self._preview_image: ImageTk.PhotoImage | None = None  # kept alive

        # ── Tkinter variables ─────────────────────────────────────────────────
        self.zoom_var = tk.DoubleVar(value=1.0)
        self.fps_var = tk.IntVar(value=30)
        self.rotation_var = tk.IntVar(value=0)
        self.resolution_var = tk.StringVar(value="720p")

        self._status_var = tk.StringVar(value="Desconectado")
        self._adb_var = tk.StringVar(value="ADB: …")

        # FakeWebcam — initialised/recreated when resolution changes
        self._fake_cam: pyfakewebcam.FakeWebcam | None = None
        self._fake_cam_res: tuple[int, int] | None = None  # (w, h)

        self._build_ui()

        # Watch resolution changes to recreate FakeWebcam
        self.resolution_var.trace_add("write", self._on_resolution_change)

        # Cleanup on exit
        atexit.register(self._cleanup)
        for sig in (signal.SIGINT, signal.SIGTERM):
            signal.signal(sig, self._signal_handler)

        self.root.protocol("WM_DELETE_WINDOW", self._on_close)

    # ── UI construction ───────────────────────────────────────────────────────

    def _build_ui(self):
        PAD = {"padx": 8, "pady": 4}

        # ── Header ────────────────────────────────────────────────────────────
        header = tk.Frame(self.root, bg="#1e1e2e")
        header.pack(fill="x")
        tk.Label(header, text="CamPC", fg="white", bg="#1e1e2e",
                 font=("Helvetica", 14, "bold")).pack(side="left", **PAD)
        self._status_label = tk.Label(header, textvariable=self._status_var,
                                      fg="#a6e3a1", bg="#1e1e2e",
                                      font=("Helvetica", 10))
        self._status_label.pack(side="left", **PAD)

        # ── Preview canvas ────────────────────────────────────────────────────
        self.canvas = tk.Canvas(self.root, width=PREVIEW_W, height=PREVIEW_H,
                                bg="black", highlightthickness=0)
        self.canvas.pack()

        # Placeholder text
        self._placeholder_id = self.canvas.create_text(
            PREVIEW_W // 2, PREVIEW_H // 2,
            text="Esperando stream del teléfono…",
            fill="#6c7086", font=("Helvetica", 12)
        )

        # ── Controls ──────────────────────────────────────────────────────────
        ctrl = tk.Frame(self.root, bg="#1e1e2e")
        ctrl.pack(fill="x", padx=8, pady=4)

        # Zoom slider
        self._add_slider(ctrl, "Zoom", self.zoom_var, 1.0, 4.0, row=0,
                         fmt=lambda v: f"{v:.1f}×")

        # FPS slider
        self._add_slider(ctrl, "FPS", self.fps_var, 5, 30, row=1,
                         fmt=lambda v: f"{int(v)}")

        # Rotation radio buttons
        tk.Label(ctrl, text="Rotación", width=8, anchor="w",
                 bg="#1e1e2e", fg="white").grid(row=2, column=0, sticky="w", pady=2)
        rot_frame = tk.Frame(ctrl, bg="#1e1e2e")
        rot_frame.grid(row=2, column=1, columnspan=2, sticky="w")
        for deg in (0, 90, 180, 270):
            tk.Radiobutton(rot_frame, text=f"{deg}°", variable=self.rotation_var,
                           value=deg, bg="#1e1e2e", fg="white",
                           selectcolor="#313244",
                           activebackground="#1e1e2e",
                           activeforeground="white").pack(side="left", padx=4)

        # Output resolution radio buttons
        tk.Label(ctrl, text="Salida", width=8, anchor="w",
                 bg="#1e1e2e", fg="white").grid(row=3, column=0, sticky="w", pady=2)
        res_frame = tk.Frame(ctrl, bg="#1e1e2e")
        res_frame.grid(row=3, column=1, columnspan=2, sticky="w")
        for label in ("720p", "1080p", "480p"):
            tk.Radiobutton(res_frame, text=label, variable=self.resolution_var,
                           value=label, bg="#1e1e2e", fg="white",
                           selectcolor="#313244",
                           activebackground="#1e1e2e",
                           activeforeground="white").pack(side="left", padx=4)

        # ── Footer ────────────────────────────────────────────────────────────
        footer = tk.Frame(self.root, bg="#181825")
        footer.pack(fill="x")
        self._adb_label = tk.Label(footer, textvariable=self._adb_var,
                                   fg="#cdd6f4", bg="#181825",
                                   font=("Helvetica", 9))
        self._adb_label.pack(side="left", **PAD)
        tk.Button(footer, text="Salir", command=self._on_close,
                  bg="#f38ba8", fg="white", relief="flat",
                  padx=8).pack(side="right", **PAD)
        tk.Button(footer, text="▶ Iniciar", command=self._start_capture,
                  bg="#a6e3a1", fg="#1e1e2e", relief="flat",
                  font=("Helvetica", 9, "bold"),
                  padx=8).pack(side="right", **PAD)
        tk.Button(footer, text="■ Detener", command=self._stop_capture,
                  bg="#fab387", fg="#1e1e2e", relief="flat",
                  padx=8).pack(side="right", **PAD)

    def _add_slider(self, parent, label, var, from_, to, row, fmt):
        tk.Label(parent, text=label, width=8, anchor="w",
                 bg="#1e1e2e", fg="white").grid(row=row, column=0, sticky="w", pady=2)
        value_label = tk.Label(parent, text=fmt(var.get()), width=6, anchor="w",
                               bg="#1e1e2e", fg="#cdd6f4")
        value_label.grid(row=row, column=2, sticky="w")

        def _update(val, lbl=value_label, f=fmt):
            lbl.config(text=f(float(val)))

        scale = ttk.Scale(parent, variable=var, from_=from_, to=to,
                          orient="horizontal", length=300, command=_update)
        scale.grid(row=row, column=1, sticky="ew", padx=4)

    # ── Capture lifecycle ─────────────────────────────────────────────────────

    def _start_capture(self):
        if self._running:
            return
        self._running = True
        self._set_status("Conectando…", "#f9e2af")
        self._capture_thread = threading.Thread(
            target=self._capture_loop, daemon=True
        )
        self._capture_thread.start()

    def _stop_capture(self):
        self._running = False
        self._set_status("Desconectado", "#f38ba8")
        with self._lock:
            self._fake_cam = None
            self._fake_cam_res = None

    def _on_resolution_change(self, *_):
        """Force FakeWebcam recreation on next frame."""
        with self._lock:
            self._fake_cam = None
            self._fake_cam_res = None

    # ── Capture loop (background thread) ─────────────────────────────────────

    def _capture_loop(self):
        while self._running:
            # ADB forward
            ok = adb_forward()
            self.root.after(0, self._adb_var.set,
                            "ADB: ● OK" if ok else "ADB: ✗ fallo")

            cap = cv2.VideoCapture(TCP_URL, cv2.CAP_FFMPEG)
            if not cap.isOpened():
                self.root.after(0, self._set_status, "Sin stream — reintentando…", "#f9e2af")
                cap.release()
                time.sleep(2)
                continue

            self.root.after(0, self._set_status, "● Conectado", "#a6e3a1")

            # Remove placeholder on first frame
            first_frame = True

            while self._running and cap.isOpened():
                ret, frame = cap.read()
                if not ret:
                    break

                if first_frame:
                    self.root.after(0, self._remove_placeholder)
                    first_frame = False

                # Read controls (thread-safe — reading primitive Python types)
                zoom = self.zoom_var.get()
                deg = self.rotation_var.get()
                res_key = self.resolution_var.get()
                target_fps = self.fps_var.get()
                out_w, out_h = RESOLUTIONS[res_key]

                # Transform
                frame = apply_zoom(frame, zoom)
                frame = apply_rotation(frame, deg)

                # v4l2 output
                out_frame = cv2.resize(frame, (out_w, out_h),
                                       interpolation=cv2.INTER_LINEAR)
                rgb_frame = cv2.cvtColor(out_frame, cv2.COLOR_BGR2RGB)
                self._write_to_v4l2(rgb_frame, out_w, out_h)

                # Preview (scaled down)
                prev = cv2.resize(frame, (PREVIEW_W, PREVIEW_H),
                                  interpolation=cv2.INTER_LINEAR)
                prev_rgb = cv2.cvtColor(prev, cv2.COLOR_BGR2RGB)
                self.root.after(0, self._update_preview, prev_rgb)

                # FPS limiter
                time.sleep(max(0.0, 1.0 / target_fps - 0.005))

            cap.release()
            if self._running:
                self.root.after(0, self._set_status, "Reconectando…", "#f9e2af")
                time.sleep(2)

        self.root.after(0, self._set_status, "Detenido", "#cdd6f4")

    def _write_to_v4l2(self, rgb_frame, w: int, h: int):
        """Write RGB frame to FakeWebcam, recreating if resolution changed."""
        with self._lock:
            if self._fake_cam is None or self._fake_cam_res != (w, h):
                try:
                    self._fake_cam = pyfakewebcam.FakeWebcam(V4L2_DEVICE, w, h)
                    self._fake_cam_res = (w, h)
                except Exception as exc:
                    print(f"[campc] FakeWebcam error: {exc}")
                    return
            try:
                self._fake_cam.schedule_frame(rgb_frame)
            except Exception as exc:
                print(f"[campc] schedule_frame error: {exc}")
                self._fake_cam = None

    # ── UI updates (always called via root.after from capture thread) ─────────

    def _update_preview(self, rgb_array):
        img = Image.fromarray(rgb_array)
        photo = ImageTk.PhotoImage(image=img)
        self._preview_image = photo  # keep reference
        self.canvas.create_image(0, 0, anchor="nw", image=photo)

    def _remove_placeholder(self):
        if self._placeholder_id is not None:
            self.canvas.delete(self._placeholder_id)
            self._placeholder_id = None

    def _set_status(self, text: str, color: str = "#cdd6f4"):
        self._status_var.set(text)
        self._status_label.config(fg=color)

    # ── Cleanup ───────────────────────────────────────────────────────────────

    def _cleanup(self):
        self._running = False
        adb_remove_forward()

    def _signal_handler(self, sig, frame):
        self._on_close()

    def _on_close(self):
        self._cleanup()
        self.root.destroy()


# ── Entry point ───────────────────────────────────────────────────────────────

def main():
    root = tk.Tk()
    root.configure(bg="#1e1e2e")
    app = CamPCApp(root)
    root.mainloop()


if __name__ == "__main__":
    main()
