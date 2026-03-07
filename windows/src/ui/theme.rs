use eframe::egui;

pub fn apply_theme(
    ctx: &egui::Context,
    base: egui::Color32,
    mantle: egui::Color32,
    surface0: egui::Color32,
    text: egui::Color32,
) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = base;
    visuals.window_fill = base;
    visuals.extreme_bg_color = mantle;
    visuals.widgets.inactive.bg_fill = surface0;
    visuals.widgets.hovered.bg_fill = surface0;
    visuals.widgets.active.bg_fill = surface0;
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, text);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, text);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, text);
    ctx.set_visuals(visuals);
}
