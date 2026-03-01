mod adb;
mod config;
mod discovery;
mod engine;
mod ffmpeg;
mod v4l2;

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use eframe::egui;

use config::{Config, ConnectionMode};
use engine::{AppState, EngineCmd, Status};
use ffmpeg::{PREVIEW_H, PREVIEW_W};

// ── Catppuccin Mocha palette ──────────────────────────────────────────────────
const C_BASE: egui::Color32 = egui::Color32::from_rgb(30, 30, 46);    // #1e1e2e
const C_MANTLE: egui::Color32 = egui::Color32::from_rgb(24, 24, 37);  // #181825
const C_SURFACE0: egui::Color32 = egui::Color32::from_rgb(49, 50, 68); // #313244
const C_TEXT: egui::Color32 = egui::Color32::from_rgb(205, 214, 244); // #cdd6f4
const C_GREEN: egui::Color32 = egui::Color32::from_rgb(166, 227, 161); // #a6e3a1
const C_YELLOW: egui::Color32 = egui::Color32::from_rgb(249, 226, 175); // #f9e2af
const C_RED: egui::Color32 = egui::Color32::from_rgb(243, 139, 168);   // #f38ba8
const C_PEACH: egui::Color32 = egui::Color32::from_rgb(250, 179, 135); // #fab387
const C_OVERLAY: egui::Color32 = egui::Color32::from_rgb(108, 112, 134); // #6c7086
const C_BLUE: egui::Color32 = egui::Color32::from_rgb(137, 180, 250);  // #89b4fa

// ── App ───────────────────────────────────────────────────────────────────────

struct CamPCApp {
    // Shared state with the engine thread
    state: Arc<Mutex<AppState>>,
    // Channel to send control commands to the engine
    cmd_tx: Sender<EngineCmd>,
    // Channel to receive decoded preview frames from FFmpeg
    preview_rx: Receiver<Vec<u8>>,
    // GPU texture for the preview canvas (updated each time a new frame arrives)
    preview_texture: Option<egui::TextureHandle>,
    // Local copy of the config (owned by GUI, sent to engine on change)
    config: Config,
    // Whether the user has clicked "Iniciar" (engine is active)
    started: bool,
    // PID of the running FFmpeg child. on_exit() kills it synchronously so it
    // doesn't become an orphan holding /dev/video10 after campc exits.
    ffmpeg_pid: Arc<Mutex<Option<u32>>>,
}

#[derive(Default)]
struct ConfigUpdate {
    stream_relevant: bool,
    zoom_only: bool,
}

impl CamPCApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);

        let config = Config::load();
        let state = Arc::new(Mutex::new(AppState {
            status: Status::Idle,
            discovered_ip: None,
        }));
        let (cmd_tx, cmd_rx) = mpsc::channel::<EngineCmd>();
        // Bounded channel: holds at most 2 preview frames. If the GUI is
        // temporarily slow, the frame reader drops the oldest frame via
        // try_send instead of growing the buffer unboundedly.
        let (preview_tx, preview_rx) = mpsc::sync_channel::<Vec<u8>>(2);
        let ffmpeg_pid: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
        let discovered = discovery::start_listener();

        engine::spawn(
            Arc::clone(&state),
            cmd_rx,
            preview_tx,
            config.clone(),
            Arc::clone(&ffmpeg_pid),
            discovered,
        );

        Self {
            state,
            cmd_tx,
            preview_rx,
            preview_texture: None,
            config,
            started: false,
            ffmpeg_pid,
        }
    }

    fn send(&self, cmd: EngineCmd) {
        let _ = self.cmd_tx.send(cmd);
    }

    fn current_status(&self) -> Status {
        self.state
            .lock()
            .map(|s| s.status.clone())
            .unwrap_or(Status::Idle)
    }

    fn current_discovered_ip(&self) -> Option<String> {
        self.state.lock().ok()?.discovered_ip.clone()
    }

    fn status_color(s: &Status) -> egui::Color32 {
        match s {
            Status::Streaming => C_GREEN,
            Status::WaitingDevice | Status::Connecting => C_YELLOW,
            Status::Error(_) => C_RED,
            Status::Idle => C_TEXT,
        }
    }

    fn refresh_preview_texture(&mut self, ctx: &egui::Context) {
        // Drain all pending frames and keep only the newest one for live preview.
        let mut latest: Option<Vec<u8>> = None;
        while let Ok(frame) = self.preview_rx.try_recv() {
            latest = Some(frame);
        }
        if let Some(frame) = latest {
            let image = egui::ColorImage::from_rgb([PREVIEW_W as usize, PREVIEW_H as usize], &frame);
            match &mut self.preview_texture {
                Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                None => {
                    self.preview_texture = Some(ctx.load_texture(
                        "preview",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
        }
    }

    fn render_header(&self, ctx: &egui::Context, status: &Status) {
        egui::TopBottomPanel::top("header")
            .frame(egui::Frame::none().fill(C_BASE).inner_margin(egui::Margin::symmetric(10.0, 6.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("CamPC")
                            .strong()
                            .size(16.0)
                            .color(C_TEXT),
                    );
                    ui.label(
                        egui::RichText::new(status.to_string())
                            .size(11.0)
                            .color(Self::status_color(status)),
                    );
                });
            });
    }

    fn apply_config_update(&mut self, update: ConfigUpdate) {
        if !(update.stream_relevant || update.zoom_only) {
            return;
        }

        self.config.save();
        if update.stream_relevant && self.started {
            self.send(EngineCmd::UpdateConfig(self.config.clone()));
        }
    }

    fn render_controls(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("controls")
            .frame(egui::Frame::none().fill(C_MANTLE).inner_margin(egui::Margin::symmetric(10.0, 8.0)))
            .show(ctx, |ui| {
                let mut update = ConfigUpdate::default();

                // FPS slider
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("FPS ").color(C_TEXT).size(11.0));
                    ui.add_space(4.0);
                    let resp = ui.add(egui::Slider::new(&mut self.config.fps, 5..=30));
                    if resp.drag_stopped() {
                        update.stream_relevant = true;
                    }
                });

                // Rotation selectors
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Rotación").color(C_TEXT).size(11.0));
                    ui.add_space(4.0);
                    for deg in [0u32, 90, 180, 270] {
                        let selected = self.config.rotation == deg;
                        if ui
                            .add(selectable_btn(&format!("{deg}°"), selected))
                            .clicked()
                        {
                            self.config.rotation = deg;
                            update.stream_relevant = true;
                        }
                    }
                });

                // Connection mode toggle
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Conexión").color(C_TEXT).size(11.0));
                    ui.add_space(4.0);
                    for (label, mode) in [("WiFi", ConnectionMode::Wifi), ("USB", ConnectionMode::Usb)] {
                        let selected = self.config.connection_mode == mode;
                        if ui.add(selectable_btn(label, selected)).clicked() {
                            self.config.connection_mode = mode;
                            update.stream_relevant = true;
                        }
                    }
                });

                // WiFi IP row (only in WiFi mode)
                if self.config.connection_mode == ConnectionMode::Wifi {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("IP:").color(C_TEXT).size(11.0));
                        ui.add_space(4.0);

                        let discovered = self.current_discovered_ip();
                        let hint = match &discovered {
                            Some(ip) => format!("{ip}  (auto)"),
                            None => "Buscando…".to_string(),
                        };

                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.config.wifi_ip)
                                .hint_text(&hint)
                                .desired_width(150.0)
                                .font(egui::FontId::monospace(11.0)),
                        );
                        if resp.lost_focus() {
                            update.stream_relevant = true;
                        }

                        if self.config.wifi_ip.is_empty() {
                            if let Some(ip) = &discovered {
                                ui.label(
                                    egui::RichText::new(ip)
                                        .color(C_BLUE)
                                        .size(11.0)
                                        .monospace(),
                                );
                            }
                        } else if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("✕").color(C_OVERLAY).size(10.0),
                                )
                                .fill(egui::Color32::TRANSPARENT)
                                .stroke(egui::Stroke::NONE),
                            )
                            .clicked()
                        {
                            self.config.wifi_ip.clear();
                            update.stream_relevant = true;
                        }
                    });
                }

                // Zoom slider (preview-only — V4L2 output remains 1280×720)
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Zoom").color(C_TEXT).size(11.0));
                    ui.add_space(4.0);
                    let resp = ui.add(
                        egui::Slider::new(&mut self.config.zoom, 1.0..=4.0)
                            .step_by(0.1)
                            .fixed_decimals(1),
                    );
                    if resp.changed() {
                        update.zoom_only = true;
                    }
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new("1×").color(C_OVERLAY).size(10.0))
                                .fill(egui::Color32::TRANSPARENT)
                                .stroke(egui::Stroke::NONE),
                        )
                        .clicked()
                    {
                        self.config.zoom = 1.0;
                        update.zoom_only = true;
                    }
                });

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // Action buttons
                ui.horizontal(|ui| {
                    if !self.started {
                        if ui
                            .add(action_btn("▶  Iniciar", C_GREEN, egui::Color32::BLACK))
                            .clicked()
                        {
                            self.started = true;
                            self.send(EngineCmd::Start);
                        }
                    } else if ui
                        .add(action_btn("■  Detener", C_PEACH, egui::Color32::BLACK))
                        .clicked()
                    {
                        self.started = false;
                        self.preview_texture = None;
                        self.send(EngineCmd::Stop);
                    }

                    ui.add_space(8.0);
                    if ui.add(action_btn("Salir", C_RED, egui::Color32::WHITE)).clicked() {
                        self.send(EngineCmd::Stop);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });

                self.apply_config_update(update);
            });
    }

    fn apply_scroll_zoom(&mut self, ui: &egui::Ui, rect: egui::Rect) {
        if !ui.rect_contains_pointer(rect) {
            return;
        }
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll == 0.0 {
            return;
        }

        self.config.zoom = (self.config.zoom * (1.0 + scroll * 0.003)).clamp(1.0, 4.0);
        self.config.save();
    }

    fn render_preview_canvas(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(egui::Color32::BLACK))
            .show(ctx, |ui| {
                let avail = ui.available_size();
                let aspect = PREVIEW_W as f32 / PREVIEW_H as f32;
                let w = avail.x.min(avail.y * aspect);
                let h = w / aspect;
                let offset = egui::vec2((avail.x - w) * 0.5, (avail.y - h) * 0.5);
                let rect = egui::Rect::from_min_size(
                    ui.min_rect().min + offset,
                    egui::vec2(w, h),
                );

                self.apply_scroll_zoom(ui, rect);
                if let Some(tex) = &self.preview_texture {
                    // UV rect stays centered so zoom always targets the middle.
                    let scale = 1.0 / self.config.zoom;
                    let margin = (1.0 - scale) * 0.5;
                    let uv = egui::Rect::from_min_max(
                        egui::pos2(margin, margin),
                        egui::pos2(1.0 - margin, 1.0 - margin),
                    );
                    ui.painter().image(tex.id(), rect, uv, egui::Color32::WHITE);
                } else {
                    let msg = if self.started {
                        "Esperando stream del teléfono…"
                    } else {
                        "Presiona ▶ Iniciar para comenzar"
                    };
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        msg,
                        egui::FontId::proportional(14.0),
                        C_OVERLAY,
                    );
                }
            });
    }
}

impl eframe::App for CamPCApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Request repaint at ~30 fps so the preview stays live
        ctx.request_repaint_after(std::time::Duration::from_millis(33));

        self.refresh_preview_texture(ctx);
        let status = self.current_status();
        self.render_header(ctx, &status);
        self.render_controls(ctx);
        self.render_preview_canvas(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Signal the engine (best-effort; it may be sleeping)
        self.send(EngineCmd::Stop);

        // Synchronously kill FFmpeg so it doesn't become an orphan holding
        // /dev/video10. The engine might not have processed Stop yet.
        if let Ok(pid_opt) = self.ffmpeg_pid.lock() {
            if let Some(pid) = *pid_opt {
                ffmpeg::kill_pid(pid);
            }
        }

        // ADB forward cleanup is only needed in USB mode.
        if self.config.connection_mode == ConnectionMode::Usb {
            adb::remove_forward(self.config.adb_port);
        }

        self.config.save();
    }
}

// ── Widget helpers ────────────────────────────────────────────────────────────

/// Small pill-style button used for rotation and mode selectors.
fn selectable_btn(label: &str, selected: bool) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| {
        let bg = if selected { C_SURFACE0 } else { C_MANTLE };
        let fg = if selected { C_TEXT } else { C_OVERLAY };
        let btn = egui::Button::new(egui::RichText::new(label).color(fg).size(11.0))
            .fill(bg)
            .stroke(egui::Stroke::new(1.0, if selected { C_TEXT } else { C_SURFACE0 }))
            .rounding(4.0);
        ui.add(btn)
    }
}

/// Coloured action button (Iniciar / Detener / Salir).
fn action_btn(
    label: &str,
    bg: egui::Color32,
    fg: egui::Color32,
) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| {
        ui.add(
            egui::Button::new(egui::RichText::new(label).color(fg).strong().size(11.0))
                .fill(bg)
                .rounding(4.0),
        )
    }
}

// ── Theme ─────────────────────────────────────────────────────────────────────

fn apply_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = C_BASE;
    visuals.window_fill = C_BASE;
    visuals.extreme_bg_color = C_MANTLE;
    visuals.widgets.inactive.bg_fill = C_SURFACE0;
    visuals.widgets.hovered.bg_fill = C_SURFACE0;
    visuals.widgets.active.bg_fill = C_SURFACE0;
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, C_TEXT);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, C_TEXT);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, C_TEXT);
    ctx.set_visuals(visuals);
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("CamPC")
            .with_inner_size([680.0, 540.0])
            .with_min_inner_size([480.0, 420.0]),
        ..Default::default()
    };

    eframe::run_native(
        "CamPC",
        options,
        Box::new(|cc| Ok(Box::new(CamPCApp::new(cc)))),
    )
}
