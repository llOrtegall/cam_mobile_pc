#!/usr/bin/env python3
"""
campc.py — CamPC Linux GUI
Streams Android rear camera to /dev/video10 (v4l2loopback) via ADB/TCP.
Requires: opencv-python, pyfakewebcam, Pillow, python3-tk
"""

import atexit
import os
import signal
import subprocess
import threading
import time
import tkinter as tk
from typing import Any, Callable
from tkinter import ttk

import cv2
import pyfakewebcam
from PIL import Image, ImageTk

# ── Stream/Device Constants ───────────────────────────────────────────────────

TCP_URL = "tcp://localhost:5000"
V4L2_DEVICE = "/dev/video10"
ADB_PORT = "5000"
ADB_TIMEOUT_S = 5

# ── Preview Constants ─────────────────────────────────────────────────────────

PREVIEW_W, PREVIEW_H = 640, 360

# ── Timing Constants ──────────────────────────────────────────────────────────

RECONNECT_DELAY_S = 2
FPS_SLEEP_FUDGE_S = 0.005

# ── UI Style Constants ────────────────────────────────────────────────────────

COLOR_BG_MAIN = "#1e1e2e"
COLOR_BG_FOOTER = "#181825"
COLOR_TEXT_LIGHT = "#cdd6f4"
COLOR_SELECT = "#313244"
COLOR_STATUS_CONNECTED = "#a6e3a1"
COLOR_STATUS_WARNING = "#f9e2af"
COLOR_STATUS_DISCONNECTED = "#f38ba8"
COLOR_STATUS_IDLE = "#cdd6f4"
UI_PAD = {"padx": 8, "pady": 4}

# ── Video Constants ───────────────────────────────────────────────────────────

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

def adb_forward() -> bool:
    """Establish ADB TCP forward; returns True on success."""
    try:
        result = subprocess.run(
            ["adb", "forward", f"tcp:{ADB_PORT}", f"tcp:{ADB_PORT}"],
            capture_output=True,
            timeout=ADB_TIMEOUT_S,
        )
        return result.returncode == 0
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return False


def adb_remove_forward() -> None:
    try:
        subprocess.run(
            ["adb", "forward", "--remove", f"tcp:{ADB_PORT}"],
            capture_output=True,
            timeout=ADB_TIMEOUT_S,
        )
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

        self.canvas: tk.Canvas
        self._status_label: tk.Label
        self._adb_label: tk.Label
        self._placeholder_id: int | None = None

        self._build_ui()

        # Watch resolution changes to recreate FakeWebcam
        self.resolution_var.trace_add("write", self._on_resolution_change)

        # Cleanup on exit
        atexit.register(self._cleanup)
        for sig in (signal.SIGINT, signal.SIGTERM):
            signal.signal(sig, self._signal_handler)

        self.root.protocol("WM_DELETE_WINDOW", self._on_close)

    # ── UI construction ───────────────────────────────────────────────────────

    def _build_ui(self) -> None:
        self._build_header()
        self._build_preview()
        self._build_controls()
        self._build_footer()

    def _build_header(self) -> None:
        header = tk.Frame(self.root, bg="#1e1e2e")
        header.pack(fill="x")
        tk.Label(header, text="CamPC", fg="white", bg="#1e1e2e",
                 font=("Helvetica", 14, "bold")).pack(side="left", **UI_PAD)
        self._status_label = tk.Label(header, textvariable=self._status_var,
                                      fg=COLOR_STATUS_CONNECTED, bg=COLOR_BG_MAIN,
                                      font=("Helvetica", 10))
        self._status_label.pack(side="left", **UI_PAD)

    def _build_preview(self) -> None:
        self.canvas = tk.Canvas(self.root, width=PREVIEW_W, height=PREVIEW_H,
                                bg="black", highlightthickness=0)
        self.canvas.pack()

        # Placeholder text
        self._placeholder_id = self.canvas.create_text(
            PREVIEW_W // 2, PREVIEW_H // 2,
            text="Esperando stream del teléfono…",
            fill="#6c7086", font=("Helvetica", 12)
        )

    def _build_controls(self) -> None:
        ctrl = tk.Frame(self.root, bg=COLOR_BG_MAIN)
        ctrl.pack(fill="x", padx=8, pady=4)

        # Zoom slider
        self._add_slider(ctrl, "Zoom", self.zoom_var, 1.0, 4.0, row=0,
                         fmt=lambda v: f"{v:.1f}×")

        # FPS slider
        self._add_slider(ctrl, "FPS", self.fps_var, 5, 30, row=1,
                         fmt=lambda v: f"{int(v)}")

        # Rotation radio buttons
        tk.Label(ctrl, text="Rotación", width=8, anchor="w",
                 bg=COLOR_BG_MAIN, fg="white").grid(row=2, column=0, sticky="w", pady=2)
        rot_frame = tk.Frame(ctrl, bg=COLOR_BG_MAIN)
        rot_frame.grid(row=2, column=1, columnspan=2, sticky="w")
        for deg in (0, 90, 180, 270):
            tk.Radiobutton(rot_frame, text=f"{deg}°", variable=self.rotation_var,
                           value=deg, bg=COLOR_BG_MAIN, fg="white",
                           selectcolor=COLOR_SELECT,
                           activebackground=COLOR_BG_MAIN,
                           activeforeground="white").pack(side="left", padx=4)

        # Output resolution radio buttons
        tk.Label(ctrl, text="Salida", width=8, anchor="w",
                 bg=COLOR_BG_MAIN, fg="white").grid(row=3, column=0, sticky="w", pady=2)
        res_frame = tk.Frame(ctrl, bg=COLOR_BG_MAIN)
        res_frame.grid(row=3, column=1, columnspan=2, sticky="w")
        for label in ("720p", "1080p", "480p"):
            tk.Radiobutton(res_frame, text=label, variable=self.resolution_var,
                           value=label, bg=COLOR_BG_MAIN, fg="white",
                           selectcolor=COLOR_SELECT,
                           activebackground=COLOR_BG_MAIN,
                           activeforeground="white").pack(side="left", padx=4)

    def _build_footer(self) -> None:
        footer = tk.Frame(self.root, bg=COLOR_BG_FOOTER)
        footer.pack(fill="x")
        self._adb_label = tk.Label(footer, textvariable=self._adb_var,
                                   fg=COLOR_TEXT_LIGHT, bg=COLOR_BG_FOOTER,
                                   font=("Helvetica", 9))
        self._adb_label.pack(side="left", **UI_PAD)
        tk.Button(footer, text="Salir", command=self._on_close,
                  bg="#f38ba8", fg="white", relief="flat",
                  padx=8).pack(side="right", **UI_PAD)
        tk.Button(footer, text="▶ Iniciar", command=self._start_capture,
                  bg="#a6e3a1", fg="#1e1e2e", relief="flat",
                  font=("Helvetica", 9, "bold"),
                  padx=8).pack(side="right", **UI_PAD)
        tk.Button(footer, text="■ Detener", command=self._stop_capture,
                  bg="#fab387", fg="#1e1e2e", relief="flat",
                  padx=8).pack(side="right", **UI_PAD)

    def _add_slider(
        self,
        parent: tk.Misc,
        label: str,
        var: tk.Variable,
        from_: float,
        to: float,
        row: int,
        fmt: Callable[[float], str],
    ) -> None:
        tk.Label(parent, text=label, width=8, anchor="w",
                 bg=COLOR_BG_MAIN, fg="white").grid(row=row, column=0, sticky="w", pady=2)
        value_label = tk.Label(parent, text=fmt(float(var.get())), width=6, anchor="w",
                               bg=COLOR_BG_MAIN, fg=COLOR_TEXT_LIGHT)
        value_label.grid(row=row, column=2, sticky="w")

        def _update(val: str, lbl: tk.Label = value_label, f: Callable[[float], str] = fmt) -> None:
            lbl.config(text=f(float(val)))

        scale = ttk.Scale(parent, variable=var, from_=from_, to=to,
                          orient="horizontal", length=300, command=_update)
        scale.grid(row=row, column=1, sticky="ew", padx=4)

    # ── Capture lifecycle ─────────────────────────────────────────────────────

    def _start_capture(self) -> None:
        if self._running:
            return
        self._running = True
        self._set_status("Conectando…", COLOR_STATUS_WARNING)
        self._capture_thread = threading.Thread(
            target=self._capture_loop, daemon=True
        )
        self._capture_thread.start()

    def _stop_capture(self) -> None:
        self._running = False
        self._set_status("Desconectado", COLOR_STATUS_DISCONNECTED)
        with self._lock:
            self._close_fake_cam()

    def _on_resolution_change(self, *_: object) -> None:
        """Force FakeWebcam recreation on next frame."""
        with self._lock:
            self._close_fake_cam()

    # ── Capture loop (background thread) ─────────────────────────────────────

    def _capture_loop(self) -> None:
        while self._running:
            self._ensure_adb_forward_and_update_ui()

            cap = self._open_stream_capture()
            if not cap.isOpened():
                self.root.after(0, self._set_status, "Sin stream — reintentando…", COLOR_STATUS_WARNING)
                cap.release()
                time.sleep(RECONNECT_DELAY_S)
                continue

            self._update_connected_state()

            # Remove placeholder after the first valid frame.
            first_frame = True

            while self._running and cap.isOpened():
                ret, frame = cap.read()
                if not ret:
                    break

                if first_frame:
                    self.root.after(0, self._remove_placeholder)
                    first_frame = False

                zoom, deg, target_fps, out_w, out_h = self._read_control_values()
                frame = self._transform_frame(frame, zoom, deg)
                self._emit_v4l2_frame(frame, out_w, out_h)
                self._emit_preview_frame(frame)
                self._sleep_for_target_fps(target_fps)

            cap.release()
            if self._running:
                self.root.after(0, self._set_status, "Reconectando…", COLOR_STATUS_WARNING)
                time.sleep(RECONNECT_DELAY_S)

        self.root.after(0, self._set_status, "Detenido", COLOR_STATUS_IDLE)

    def _ensure_adb_forward_and_update_ui(self) -> None:
        ok = adb_forward()
        self.root.after(0, self._adb_var.set, "ADB: ● OK" if ok else "ADB: ✗ fallo")

    def _open_stream_capture(self) -> cv2.VideoCapture:
        return cv2.VideoCapture(TCP_URL, cv2.CAP_FFMPEG)

    def _update_connected_state(self) -> None:
        self.root.after(0, self._set_status, "● Conectado", COLOR_STATUS_CONNECTED)

    def _read_control_values(self) -> tuple[float, int, int, int, int]:
        """Read control values from Tk variables."""
        zoom = float(self.zoom_var.get())
        deg = int(self.rotation_var.get())
        target_fps = int(self.fps_var.get())
        out_w, out_h = RESOLUTIONS[self.resolution_var.get()]
        return zoom, deg, target_fps, out_w, out_h

    def _transform_frame(self, frame: Any, zoom: float, rotation: int) -> Any:
        frame = apply_zoom(frame, zoom)
        frame = apply_rotation(frame, rotation)
        return frame

    def _emit_v4l2_frame(self, frame: Any, out_w: int, out_h: int) -> None:
        out_frame = cv2.resize(frame, (out_w, out_h), interpolation=cv2.INTER_LINEAR)
        rgb_frame = cv2.cvtColor(out_frame, cv2.COLOR_BGR2RGB)
        self._write_to_v4l2(rgb_frame, out_w, out_h)

    def _emit_preview_frame(self, frame: Any) -> None:
        prev = cv2.resize(frame, (PREVIEW_W, PREVIEW_H), interpolation=cv2.INTER_LINEAR)
        prev_rgb = cv2.cvtColor(prev, cv2.COLOR_BGR2RGB)
        self.root.after(0, self._update_preview, prev_rgb)

    def _sleep_for_target_fps(self, target_fps: int) -> None:
        time.sleep(max(0.0, 1.0 / target_fps - FPS_SLEEP_FUDGE_S))

    def _close_fake_cam(self) -> None:
        if self._fake_cam is not None:
            try:
                # Intentionally close the private fd for reliable release.
                os.close(self._fake_cam._video_device)
            except Exception:
                pass
            self._fake_cam = None
            self._fake_cam_res = None

    def _write_to_v4l2(self, rgb_frame: Any, w: int, h: int) -> None:
        """Write RGB frame to FakeWebcam, recreating if resolution changed."""
        with self._lock:
            if self._fake_cam is None or self._fake_cam_res != (w, h):
                self._close_fake_cam()
                try:
                    self._fake_cam = pyfakewebcam.FakeWebcam(V4L2_DEVICE, w, h)
                    # Read back actual dimensions accepted by the device
                    # (may differ from requested if a reader is already connected)
                    actual_w = self._fake_cam._settings.fmt.pix.width
                    actual_h = self._fake_cam._settings.fmt.pix.height
                    self._fake_cam_res = (actual_w, actual_h)
                    if (actual_w, actual_h) != (w, h):
                        print(f"[campc] Device locked to {actual_w}×{actual_h}, requested {w}×{h}")
                except Exception as exc:
                    print(f"[campc] FakeWebcam error: {exc}")
                    self._fake_cam = None
                    return

            # Resize frame to actual device dimensions before sending
            actual_w, actual_h = self._fake_cam_res
            if rgb_frame.shape[1] != actual_w or rgb_frame.shape[0] != actual_h:
                rgb_frame = cv2.resize(rgb_frame, (actual_w, actual_h),
                                       interpolation=cv2.INTER_LINEAR)
            try:
                self._fake_cam.schedule_frame(rgb_frame)
            except Exception as exc:
                print(f"[campc] schedule_frame error: {exc}")
                self._close_fake_cam()

    # ── UI updates (always called via root.after from capture thread) ─────────

    def _update_preview(self, rgb_array: Any) -> None:
        img = Image.fromarray(rgb_array)
        photo = ImageTk.PhotoImage(image=img)
        self._preview_image = photo  # keep reference
        self.canvas.create_image(0, 0, anchor="nw", image=photo)

    def _remove_placeholder(self) -> None:
        if self._placeholder_id is not None:
            self.canvas.delete(self._placeholder_id)
            self._placeholder_id = None

    def _set_status(self, text: str, color: str = COLOR_STATUS_IDLE) -> None:
        self._status_var.set(text)
        self._status_label.config(fg=color)

    # ── Cleanup ───────────────────────────────────────────────────────────────

    def _cleanup(self) -> None:
        self._running = False
        adb_remove_forward()

    def _signal_handler(self, sig: int, frame: Any) -> None:
        self._on_close()

    def _on_close(self) -> None:
        self._cleanup()
        self.root.destroy()


# ── Entry point ───────────────────────────────────────────────────────────────

def main():
    root = tk.Tk()
    root.configure(bg=COLOR_BG_MAIN)
    app = CamPCApp(root)
    root.mainloop()


if __name__ == "__main__":
    main()
