use std::sync::Arc;
use std::time::Duration;

use eframe::egui;

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
            ui.vertical_centered(|ui| {
                ui.add_space(8.0);
                ui.heading("ReCast");
                ui.add_space(12.0);

                let mut enabled = self.control.is_enabled();
                if ui
                    .add(egui::Checkbox::new(&mut enabled, "Enabled"))
                    .changed()
                {
                    self.control.set_enabled(enabled);
                }

                ui.add_space(12.0);
                ui.label(format!("Words fixed: {}", self.control.fixed_count()));
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
            .with_inner_size([300.0, 200.0])
            .with_resizable(false)
            .with_icon(icon_data),
        ..Default::default()
    };
    eframe::run_native(
        "ReCast",
        opts,
        Box::new(|_cc| Box::new(App { control })),
    )
}
