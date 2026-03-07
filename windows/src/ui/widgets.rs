use eframe::egui;

/// Small pill-style button used for rotation and mode selectors.
pub fn selectable_btn(
    label: &str,
    selected: bool,
    selected_bg: egui::Color32,
    idle_bg: egui::Color32,
    selected_fg: egui::Color32,
    idle_fg: egui::Color32,
    border: egui::Color32,
) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| {
        let bg = if selected { selected_bg } else { idle_bg };
        let fg = if selected { selected_fg } else { idle_fg };
        let btn = egui::Button::new(egui::RichText::new(label).color(fg).size(11.0))
            .fill(bg)
            .stroke(egui::Stroke::new(1.0, if selected { selected_fg } else { border }))
            .rounding(4.0);
        ui.add(btn)
    }
}

/// Coloured action button (Start / Stop / Exit).
pub fn action_btn(label: &str, bg: egui::Color32, fg: egui::Color32) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| {
        ui.add(
            egui::Button::new(egui::RichText::new(label).color(fg).strong().size(11.0))
                .fill(bg)
                .rounding(4.0),
        )
    }
}
