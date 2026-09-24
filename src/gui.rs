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
    show_log: bool,
    health: String,
    show_practice: bool,
    practice_text: String,
    practice_offered: bool,
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
            self.health = crate::platform::status(&self.control);
            self.control.live_log.observe_health(&self.health);
        }
        if !self.practice_offered
            && self
                .control
                .listener_ready
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            self.practice_offered = true;
            if crate::practice::first_run() {
                self.show_practice = true;
                crate::practice::opened(&self.control);
            }
        }
        egui::Window::new("Practice ReCast")
            .open(&mut self.show_practice)
            .show(ctx, |ui| {
                ui.label(crate::practice::instructions());
                ui.add(
                    egui::TextEdit::singleline(&mut self.practice_text).hint_text("Practice here").interactive(crate::layout::focus_supported()),
                );
                if !crate::layout::focus_supported() {
                    ui.label("Live practice needs application detection. Use a supported Hyprland, Sway, or X11 session; ordinary correction keeps its existing behavior.");
                }
                ui.label(crate::practice::feedback(&self.control));
                ui.label(&self.health);
                ui.label("Close to skip. Reopen Practice from the controls anytime.");
            });
        self.control
            .practice_open
            .store(self.show_practice, std::sync::atomic::Ordering::Relaxed);

        egui::Window::new("Typing shortcuts")
            .open(&mut self.show_shortcuts)
            .show(ctx, |ui| {
                ui.label(crate::notify::shortcuts());
            });

        let was_showing_log = self.show_log;
        egui::Window::new("Live log")
            .open(&mut self.show_log)
            .default_size([720.0, 400.0])
            .show(ctx, |ui| {
                if self.control.live_log.active() {
                    if ui.button("Stop logging").clicked() {
                        self.control.live_log.stop();
                    }
                } else if ui.button("Start logging").clicked() {
                    self.control.live_log.start();
                    self.control.live_log.observe_health(&self.health);
                }
                ui.small("Memory only. Closing this window stops logging. New sessions clear earlier entries.");
                egui::ScrollArea::both().stick_to_bottom(true).show(ui, |ui| {
                    ui.monospace(self.control.live_log.text());
                });
            });
        if was_showing_log && !self.show_log {
            self.control.live_log.close();
        }

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
                let (state, detail) = self.health.split_once(" — ").unwrap_or((&self.health, ""));
                ui.heading(state);
                ui.label(detail);

                // Keep the saved switch distinct from the temporary pause.
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
                ui.small("Turning this off keeps correction disabled after restarting ReCast.");

                ui.group(|ui| {
                ui.label("Pause correction");
                ui.add_enabled_ui(enabled, |ui| {
                if let Some(left) = self.control.pause_remaining() {
                    if ui.button(format!("Resume (paused, {} min left)", left.as_secs() / 60 + 1)).clicked() {
                        self.control.resume();
                    }
                } else if ui.button("Pause for 30 minutes").clicked() {
                    self.control.pause_for(Duration::from_secs(30 * 60));
                }

                if let Some(id) = self.control.paused_app() {
                    if ui.button(format!("Resume in {id}")).clicked() {
                        self.control.resume_app();
                    }
                } else if let Some((name, id)) = &self.last_app {
                    if ui.button(format!("Pause in {name} until I switch away")).clicked() {
                        self.control.pause_in_app(id);
                    }
                } else {
                    ui.add_enabled(false, egui::Button::new("Pause in application (waiting for focus)"));
                }
                });
                });

                ui.add_space(12.0);
                ui.label(
                    RichText::new(format!("Words fixed: {}", self.control.fixed_count()))
                        .size(14.0)
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
                let history = self.control.history();
                ui.collapsing(format!("Recent corrections ({})", history.len()), |ui| {
                    ui.label("Click a correction to stop correcting that word.");
                    if history.is_empty() {
                        ui.label("No corrections yet.");
                    }
                    for correction in history {
                        let ignored = crate::complete::ignored(&correction.from);
                        let label = format!("{}{} → {} ({}){}",
                            if correction.undone { "Undone: " } else { "" },
                            correction.from, correction.to, correction.kind.tag(),
                            if ignored { " — ignored" } else { "" });
                        if ui.add_enabled(!ignored, egui::Button::new(label)).clicked() {
                            crate::complete::ignore_word(&correction.from);
                        }
                    }
                });
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
                for (label, key, current, choices) in [
                    ("Double-tap action (undo / convert)", "action_shortcut", &cfg.action_shortcut, crate::config::ACTION_SHORTCUTS),
                    ("Completion (single tap)", "completion_shortcut", &cfg.completion_shortcut, crate::config::MODIFIER_SHORTCUTS),
                ] {
                    let mut selected = current.clone();
                    egui::ComboBox::from_id_source(key).selected_text(crate::config::modifier_label(&selected)).show_ui(ui, |ui| {
                        for &(value, text) in choices {
                            ui.selectable_value(&mut selected, value.into(), text);
                        }
                    });
                    ui.label(label);
                    if &selected != current {
                        self.error = crate::settings::set_live(&self.control, key, &selected).err();
                    }
                }
                ui.label("Extra undo shortcut (single tap):");
                let mut shortcut = cfg.undo_shortcut.clone();
                egui::ComboBox::from_id_source("undo_shortcut").selected_text(crate::practice::shortcut_label(&shortcut)).show_ui(ui, |ui| {
                    for value in ["none", "left_ctrl", "right_ctrl"] {
                        ui.selectable_value(&mut shortcut, value.into(), crate::practice::shortcut_label(value));
                    }
                });
                if shortcut != cfg.undo_shortcut {
                    self.error = crate::settings::set_live(&self.control, "undo_shortcut", &shortcut).err();
                }
                ui.separator();
                ui.heading("Application modes");
                let mut excluded = crate::types::lock_forgiving(&self.control.excluded_apps).clone();
                excluded.extend(crate::types::lock_forgiving(&self.control.layout_only_apps).iter().cloned());
                excluded.sort(); excluded.dedup();
                if !crate::layout::focus_supported() {
                    ui.label("Application detection is unavailable. Saved restrictions pause correction when the app is unknown. Restore Full below to remove a restriction.");
                } else if let Some((name, id)) = &self.last_app {
                    ui.label(name).on_hover_text(id);
                    for mode in crate::config::AppMode::ALL {
                        if ui.selectable_label(self.control.app_mode(Some(id)) == Some(mode), mode.label()).clicked() {
                            self.error = crate::settings::set_app_mode(&self.control, id, mode).err();
                        }
                    }
                } else {
                    ui.label("Switch to the app you want to configure, then return here.");
                }
                for id in excluded {
                    if ui.button(format!("{id}: {} — restore Full", self.control.app_mode(Some(&id)).unwrap().label())).clicked() {
                        self.error = crate::settings::set_app_mode(&self.control, &id, crate::config::AppMode::Full).err();
                    }
                }
                if let Some(error) = &self.error {
                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                }
                if ui.button("Typing shortcuts…").clicked() {
                    self.show_shortcuts = true;
                }
                if ui.button("Live log…").clicked() {
                    self.show_log = true;
                }
                if ui.button("Practice correction and undo…").clicked() {
                    self.show_practice = true;
                    self.practice_text.clear();
                    crate::practice::opened(&self.control);
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
                show_log: false,
                health: "Starting…".into(),
                show_practice: false,
                practice_text: String::new(),
                practice_offered: false,
            })
        }),
    )
}
