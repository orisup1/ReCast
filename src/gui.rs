use std::sync::Arc;
use std::time::Duration;

use eframe::egui;
use eframe::egui::RichText;

use crate::types::AppControl;

struct App {
    control: Arc<AppControl>,
    last_app: Option<(String, String)>,
    last_app_check: std::time::Instant,
    error: Option<String>,
    show_shortcuts: bool,
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Counter + checkbox state come from another thread; repaint to keep
        // the displayed count in sync without driving CPU when idle.
        ctx.request_repaint_after(Duration::from_millis(250));
        if self.last_app_check.elapsed() >= Duration::from_secs(1) {
            if let Some(app) = crate::platform::active_application() {
                self.last_app = Some(app);
            }
            self.last_app_check = std::time::Instant::now();
        }

        egui::Window::new("Typing shortcuts")
            .open(&mut self.show_shortcuts)
            .show(ctx, |ui| {
                ui.label(crate::notify::SHORTCUTS);
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
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
                ui.separator();
                ui.heading("Settings");
                let cfg = crate::config::Config::global();
                for (label, key, mut checked) in [
                    ("Correct English spelling", "spell", cfg.spell_enabled),
                    ("Word completion and abbreviations", "complete", cfg.complete_enabled),
                    ("Conservative spelling (single-typo fixes)", "spell_dist", cfg.spell_max_dist == 1),
                ] {
                    if ui.checkbox(&mut checked, label).changed() {
                        let value = if key == "spell_dist" { if checked { "1" } else { "3" } } else if checked { "true" } else { "false" };
                        self.error = crate::settings::set_live(&self.control, key, value).err();
                    }
                }
                ui.label("Changes are saved and applied immediately.");
                ui.separator();
                ui.heading("Excluded applications");
                let excluded = crate::types::lock_forgiving(&self.control.excluded_apps).clone();
                if !crate::layout::focus_supported() {
                    ui.label("Application detection is unavailable in this session. Adding exclusions would pause all corrections. Existing exclusions can still be removed below.");
                } else if let Some((name, id)) = &self.last_app {
                    let verb = if excluded.contains(&id.to_lowercase()) { "Allow" } else { "Exclude" };
                    if ui.button(format!("{verb} {name}")).on_hover_text(id).clicked() {
                        self.error = crate::platform::toggle_app_exclusion(&self.control, id).err();
                    }
                } else {
                    ui.label("Switch to the app you want to exclude, then return here.");
                }
                for id in excluded {
                    if ui.button(format!("Allow {id}")).clicked() {
                        self.error = crate::platform::toggle_app_exclusion(&self.control, &id).err();
                    }
                }
                if let Some(error) = &self.error {
                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                }
                if ui.button("Typing shortcuts…").clicked() {
                    self.show_shortcuts = true;
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
            .with_inner_size([430.0, 560.0])
            .with_resizable(true)
            .with_icon(icon_data),
        ..Default::default()
    };
    eframe::run_native(
        "ReCast",
        opts,
        Box::new(|_cc| {
            Box::new(App {
                control,
                last_app: None,
                last_app_check: std::time::Instant::now(),
                error: None,
                show_shortcuts: false,
            })
        }),
    )
}
