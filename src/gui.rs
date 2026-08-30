use std::sync::Arc;
use std::time::Duration;

use eframe::egui;
use eframe::egui::RichText;

use crate::types::AppControl;

struct App {
    control: Arc<AppControl>,
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Counter + checkbox state come from another thread; repaint to keep
        // the displayed count in sync without driving CPU when idle.
        ctx.request_repaint_after(Duration::from_millis(250));

        egui::CentralPanel::default().show(ctx, |ui| {
            // Fixed size content, no scrolling
            ui.vertical_centered(|ui| {
                ui.add_space(8.0);
                ui.heading(
                    RichText::new("ReCast")
                        .size(24.0)
                        .strong()
                        .color(egui::Color32::LIGHT_GRAY),
                );
                ui.add_space(12.0);

                // The switch itself, not `is_enabled()`, which also reads false
                // during a pause — this window has no pause control, so a
                // checkbox that unticked itself for half an hour would be
                // reporting something the user can't act on here.
                let mut enabled = self.control.is_switched_on();
                let checkbox = egui::Checkbox::new(
                    &mut enabled,
                    egui::RichText::new("Enable correction").color(egui::Color32::LIGHT_GRAY),
                );
                let response = ui.add(checkbox);
                if response.changed() {
                    self.control.set_enabled(enabled);
                }
                response.on_hover_ui(|ui| {
                    ui.label("Layout switching, spelling and completion — the one switch for all three");
                });

                ui.add_space(12.0);
                ui.label(
                    RichText::new(format!("Words fixed: {}", self.control.fixed_count()))
                        .size(18.0)
                        .color(egui::Color32::LIGHT_GRAY),
                );
                // Shown next to the fixed count, and only once there is
                // something to show: the pair is what says whether the speller
                // is set where this user wants it.
                let undone = self.control.undo_count();
                if undone > 0 {
                    ui.label(
                        RichText::new(format!("Taken back: {undone}"))
                            .size(14.0)
                            .color(egui::Color32::GRAY),
                    );
                }
                if let Some(hint) = self.control.tighten_hint() {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(hint)
                            .small()
                            .color(egui::Color32::from_rgb(220, 190, 90)),
                    );
                }
                ui.add_space(16.0);
                // `--window` runs ReCast in the foreground with the listener on
                // a background thread, so closing this window ends the process.
                // The label used to claim the opposite, which is the worst way
                // to find out: you close it and corrections stop.
                ui.label(
                    RichText::new("Closing this window quits ReCast.\nRun `make service` to keep it running at login.")
                        .small()
                        .italics()
                        .color(egui::Color32::LIGHT_GRAY),
                );
            });
        });
    }
}

pub fn run(control: Arc<AppControl>) -> Result<(), eframe::Error> {
    // Load the same icon as used for the tray (32x32 RGBA)
    const ICON_RGBA: &[u8] = include_bytes!("../assets/tray-icon.rgba");
    let icon_data = egui::IconData {
        rgba: ICON_RGBA.to_vec(),
        width: 32,
        height: 32,
    };

    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([320.0, 240.0])
            .with_resizable(false)
            .with_icon(icon_data),
        ..Default::default()
    };
    eframe::run_native("ReCast", opts, Box::new(|_cc| Box::new(App { control })))
}
