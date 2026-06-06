use std::sync::Arc;
use std::time::Duration;

use eframe::egui;
use eframe::egui::RichText;
use eframe::egui::Layout;

use crate::types::AppControl;

struct App {
    control: Arc<AppControl>,
    about_open: bool,
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Counter + checkbox state come from another thread; repaint to keep
        // the displayed count in sync without driving CPU when idle.
        ctx.request_repaint_after(Duration::from_millis(250));

        // Menu button to open about popup
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("?").clicked() {
                    self.about_open = true;
                }
            });
        });

        // About popup window - conditionally show and handle close button
        {
            let is_open = self.about_open;
            if is_open {
                egui::Window::new("About ReCast")
                    .show(ctx, |ui| {
                        ui.vertical(|ui| {
                            ui.heading("ReCast");
                            ui.label("Layout mistake fixer for bilingual typing.");
                            ui.add_space(8.0);
                            ui.hyperlink_to("https://github.com/orisupino/reCast", "Source code");
                            ui.add_space(8.0);
                            if let Some(version) = option_env!("CARGO_PKG_VERSION") {
                                ui.label(format!("Version: {}", version));
                            }
                            ui.add_space(8.0);
                            if ui.button("Close").clicked() {
                                self.about_open = false;
                            }
                        });
                    });
            }
        }

        // Main panel with controls and info
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(8.0);
                ui.heading(
                    RichText::new("ReCast")
                        .size(24.0)
                        .strong(),
                );
                ui.add_space(12.0);

                let mut enabled = self.control.is_enabled();
                let checkbox = egui::Checkbox::new(
                    &mut enabled,
                    egui::RichText::new("Enable layout correction")
                        .color(egui::Color32::LIGHT_GRAY),
                );
                let response = ui.add(checkbox);
                response.on_hover_ui(|ui| {
                    ui.label("Turn layout correction on or off");
                });

                ui.add_space(12.0);
                ui.label(
                    RichText::new(format!("Words fixed: {}", self.control.fixed_count()))
                        .size(18.0),
                );
                ui.add_space(16.0);
                ui.label(
                    RichText::new("Running in background. Closes window but keeps service.")
                        .small()
                        .italics(),
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
            .with_inner_size([340.0, 320.0])
            .with_resizable(true)
            .with_min_inner_size([300.0, 250.0])
            .with_icon(icon_data),
        ..Default::default()
    };
    eframe::run_native(
        "ReCast",
        opts,
        Box::new(|_cc| Box::new(App {
            control,
            about_open: false,
        })),
    )
}