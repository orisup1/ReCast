//! Modeless settings pages. All changes use the same validated, atomic saves as the tray.

use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};

use super::desktop::{Element as E, Kind as K, Window as Form};
use crate::config::{AppMode, Config, LanguagePreference};
use crate::types::AppControl;

pub enum Action {
    OpenConfig,
    Edit(crate::complete::RuleKind),
}

pub struct Window {
    pub form: Form,
    page: usize,
    applications: Vec<(String, String)>,
    selected_app: usize,
    message: String,
    preview: String,
}

fn choice(
    id: u16,
    label: &str,
    values: &[(&str, &str)],
    selected: &str,
    y: f64,
    width: f64,
) -> Vec<E> {
    vec![
        E::new(id + 1000, K::Label, label, [24.0, y, width, 24.0]),
        E::new(
            id,
            K::Choice(
                values.iter().map(|(_, label)| (*label).into()).collect(),
                values
                    .iter()
                    .position(|(value, _)| *value == selected)
                    .unwrap_or(0),
            ),
            label,
            [24.0, y + 27.0, width, 32.0],
        ),
    ]
}

impl Window {
    pub fn new(
        target: &EventLoopWindowTarget<()>,
        proxy: EventLoopProxy<()>,
        control: &AppControl,
        application: Option<&(String, String)>,
    ) -> Result<Self, String> {
        let form = Form::new(
            target,
            proxy,
            "ReCast Settings",
            [720.0, 570.0],
            [660.0, 460.0],
        )?;
        let mut window = Self {
            form,
            page: 0,
            applications: Vec::new(),
            selected_app: 0,
            message: "Changes save and apply immediately.".into(),
            preview: "Choose an example to see what your current settings would do.".into(),
        };
        window.refresh_apps(control, application);
        window.render(control)?;
        window.form.show();
        Ok(window)
    }

    fn refresh_apps(&mut self, control: &AppControl, application: Option<&(String, String)>) {
        let mut apps = Vec::new();
        if let Some(app) = application {
            apps.push(app.clone());
        }
        let mut saved = crate::types::lock_forgiving(&control.excluded_apps).clone();
        saved.extend(
            crate::types::lock_forgiving(&control.layout_only_apps)
                .iter()
                .cloned(),
        );
        saved.extend(
            crate::types::lock_forgiving(&control.manual_only_apps)
                .iter()
                .cloned(),
        );
        saved.sort();
        saved.dedup();
        for id in saved {
            if !apps
                .iter()
                .any(|(_, known)| known.eq_ignore_ascii_case(&id))
            {
                apps.push((id.clone(), id));
            }
        }
        self.applications = apps;
        self.selected_app = 0;
    }

    pub fn update(
        &mut self,
        control: &AppControl,
        application: Option<&(String, String)>,
    ) -> Result<Vec<Action>, String> {
        let mut commands = Vec::new();
        for action in self.form.actions() {
            let result = match action {
                10..=13 => {
                    self.page = (action - 10) as usize;
                    self.message = "Changes save and apply immediately.".into();
                    if self.page == 2 {
                        self.refresh_apps(control, application);
                    }
                    break;
                }
                100 => {
                    control.set_enabled(self.form.checked(100));
                    Ok(())
                }
                101 | 102 => crate::settings::set_live(
                    control,
                    if action == 101 { "spell" } else { "complete" },
                    if self.form.checked(action) {
                        "true"
                    } else {
                        "false"
                    },
                ),
                103 => crate::settings::set_live(
                    control,
                    "spell_dist",
                    &self.form.selection(103).to_string(),
                ),
                104 => {
                    if crate::prefs::set_autostart(self.form.checked(104)) {
                        Ok(())
                    } else {
                        Err("Could not change Start at login. Your previous setting is still shown.".into())
                    }
                }
                107 => {
                    let layout = if self.form.selection(109) == 0 {
                        crate::types::Language::English
                    } else {
                        crate::types::Language::Hebrew
                    };
                    self.preview = match crate::explain::preview(
                        crate::explain::EXAMPLES
                            .get(self.form.selection(108))
                            .copied()
                            .unwrap_or("recieve"),
                        layout,
                        None,
                    ) {
                        Ok((word, reason, _)) => {
                            format!("Result: \u{2068}{word}\u{2069}\n{reason}")
                        }
                        Err(error) => error,
                    };
                    break;
                }
                105 => {
                    commands.push(Action::OpenConfig);
                    Ok(())
                }
                106 => LanguagePreference::ALL
                    .get(self.form.selection(106))
                    .ok_or_else(|| "Choose a language preference.".to_string())
                    .and_then(|preference| {
                        crate::settings::set_live(control, "prefer", preference.value())
                    }),
                201..=204 => {
                    let (key, values) = match action {
                        201 => ("action_shortcut", crate::config::ACTION_SHORTCUTS),
                        202 => ("completion_shortcut", crate::config::MODIFIER_SHORTCUTS),
                        203 => ("correction_shortcut", crate::config::MODIFIER_SHORTCUTS),
                        _ => ("undo_shortcut", &crate::config::MODIFIER_SHORTCUTS[..3]),
                    };
                    values
                        .get(self.form.selection(action))
                        .ok_or_else(|| "Choose a shortcut.".to_string())
                        .and_then(|(value, _)| crate::settings::set_live(control, key, value))
                }
                301 => {
                    self.selected_app = self.form.selection(301);
                    // Render this application's mode before accepting another action.
                    break;
                }
                302 => {
                    if let (Some((_, id)), Some(mode)) = (
                        self.applications.get(self.selected_app),
                        AppMode::ALL.get(self.form.selection(302)),
                    ) {
                        crate::settings::set_app_mode(control, id, *mode)
                    } else {
                        Err(
                            "Switch to an application first, then choose Use last application."
                                .into(),
                        )
                    }
                }
                303 => {
                    self.refresh_apps(control, application);
                    break;
                }
                401..=403 => {
                    commands.push(Action::Edit(
                        crate::complete::RuleKind::ALL[(action - 401) as usize],
                    ));
                    Ok(())
                }
                404 => {
                    crate::complete::reload_user_files();
                    Ok(())
                }
                _ => Ok(()),
            };
            self.message =
                result.map_or_else(|error| error, |_| "Saved. Changes are active now.".into());
        }
        self.render(control)?;
        Ok(commands)
    }

    fn render(&mut self, control: &AppControl) -> Result<(), String> {
        let [width, _] = self.form.size();
        let height = if self.page == 0 { 840.0 } else { 670.0 };
        let content = width - 48.0;
        let config = Config::global();
        let mut scene = vec![E::new(
            1,
            K::Heading,
            "Settings",
            [24.0, 20.0, content, 30.0],
        )];
        for (index, title) in ["General", "Shortcuts", "Applications", "Word rules"]
            .iter()
            .enumerate()
        {
            scene.push(E::new(
                10 + index as u16,
                K::Button,
                if self.page == index {
                    format!("{title} (selected)")
                } else {
                    title.to_string()
                },
                [
                    24.0 + index as f64 * content / 4.0,
                    70.0,
                    content / 4.0 - 8.0,
                    34.0,
                ],
            ));
        }
        match self.page {
            0 => {
                scene.extend([
                    E::new(
                        100,
                        K::Check(control.is_switched_on()),
                        "Enable correction",
                        [24.0, 138.0, content, 30.0],
                    ),
                    E::new(
                        101,
                        K::Check(config.spell_enabled),
                        "Correct English and Hebrew spelling",
                        [24.0, 180.0, content, 30.0],
                    ),
                    E::new(
                        102,
                        K::Check(config.complete_enabled),
                        "Word completion and abbreviations",
                        [24.0, 222.0, content, 30.0],
                    ),
                    E::new(
                        3,
                        K::Label,
                        "Language preference",
                        [24.0, 264.0, content, 24.0],
                    ),
                    E::new(
                        106,
                        K::Choice(
                            LanguagePreference::ALL
                                .iter()
                                .map(|preference| preference.label().into())
                                .collect(),
                            LanguagePreference::ALL
                                .iter()
                                .position(|preference| *preference == config.language_preference)
                                .unwrap_or(0),
                        ),
                        "Language preference",
                        [24.0, 292.0, content, 32.0],
                    ),
                    E::new(
                        5,
                        K::Label,
                        "Spelling sensitivity",
                        [24.0, 338.0, content, 24.0],
                    ),
                    E::new(
                        103,
                        K::Choice(
                            vec![
                                "No automatic spelling edits".into(),
                                "Conservative — common typos only".into(),
                                "Balanced — everyday spelling mistakes".into(),
                                "Flexible — more complex typos".into(),
                            ],
                            config.spell_max_dist.min(3) as usize,
                        ),
                        "Spelling sensitivity",
                        [24.0, 366.0, content, 32.0],
                    ),
                    E::new(
                        105,
                        K::Button,
                        "Advanced settings…",
                        [24.0, 454.0, 190.0, 32.0],
                    ),
                    E::new(
                        4,
                        K::Label,
                        "Config file edits require restarting ReCast.",
                        [236.0, 454.0, content - 212.0, 32.0],
                    ),
                ]);
                if let Some(enabled) = crate::prefs::autostart_enabled() {
                    scene.push(E::new(
                        104,
                        K::Check(enabled),
                        "Start at login",
                        [24.0, 410.0, content, 30.0],
                    ));
                }
            }
            1 => {
                for (id, label, values, selected, y) in [
                    (
                        204,
                        "Undo only (single tap)",
                        &crate::config::MODIFIER_SHORTCUTS[..3],
                        config.undo_shortcut.as_str(),
                        130.0,
                    ),
                    (
                        201,
                        "Undo or convert (double tap)",
                        crate::config::ACTION_SHORTCUTS,
                        config.action_shortcut.as_str(),
                        215.0,
                    ),
                    (
                        202,
                        "Complete a word (single tap)",
                        crate::config::MODIFIER_SHORTCUTS,
                        config.completion_shortcut.as_str(),
                        300.0,
                    ),
                    (
                        203,
                        "Spelling suggestions (single tap)",
                        crate::config::MODIFIER_SHORTCUTS,
                        config.correction_shortcut.as_str(),
                        385.0,
                    ),
                ] {
                    scene.extend(choice(id, label, values, selected, y, content));
                }
            }
            2 => {
                let app = self.applications.get(self.selected_app);
                scene.extend([
                    E::new(3, K::Label, "Application", [24.0, 136.0, content, 24.0]),
                    E::new(301, K::Choice(if self.applications.is_empty() { vec!["No application detected".into()] } else { self.applications.iter().map(|(name, _)| name.clone()).collect() }, self.selected_app), "Application", [24.0, 164.0, content, 32.0]).enabled(app.is_some()),
                    E::new(4, K::Label, app.map_or("Switch to an app, then return here.".into(), |(_, id)| format!("Identifier: {id}")), [24.0, 208.0, content, 44.0]),
                    E::new(5, K::Label, "Correction mode", [24.0, 266.0, content, 24.0]),
                    E::new(302, K::Choice(AppMode::ALL.iter().map(|m| m.label().into()).collect(), app.and_then(|(_, id)| control.app_mode(Some(id))).and_then(|mode| AppMode::ALL.iter().position(|m| *m == mode)).unwrap_or(0)), "Correction mode", [24.0, 294.0, content, 32.0]).enabled(app.is_some()),
                    E::new(6, K::Label, "Layout only skips spelling, abbreviations, and completion. Manual only waits for a shortcut. Off pauses all correction in this application.", [24.0, 342.0, content, 58.0]),
                    E::new(303, K::Button, "Use last application", [24.0, 418.0, 210.0, 32.0]),
                ]);
            }
            _ => {
                for (index, kind) in crate::complete::RuleKind::ALL.iter().enumerate() {
                    let y = 136.0 + index as f64 * 98.0;
                    scene.push(E::new(
                        401 + index as u16,
                        K::Button,
                        format!("{}…", kind.title()),
                        [24.0, y, 240.0, 32.0],
                    ));
                    scene.push(E::new(
                        20 + index as u16,
                        K::Label,
                        [
                            "Words ReCast should leave unchanged.",
                            "Saved exceptions from repeated undos.",
                            "Custom shortcuts and their expansions.",
                        ][index],
                        [24.0, y + 40.0, content, 44.0],
                    ));
                }
                scene.push(E::new(
                    404,
                    K::Button,
                    "Reload lists",
                    [24.0, 444.0, 150.0, 32.0],
                ));
            }
        }
        if self.page == 0 {
            scene.extend([
                E::new(30, K::Label, "Flexible may change unfamiliar words. Short words keep stricter limits. Turn spelling on to apply automatic spelling fixes.", [24.0, 502.0, content, 52.0]),
                E::new(31, K::Label, "Preview an example (current settings; no typing or learning)", [24.0, 548.0, content, 24.0]),
                E::new(110, K::Label, "Example word", [24.0, 578.0, content - 290.0, 24.0]),
                E::new(108, K::Choice(crate::explain::EXAMPLES.iter().map(|s| s.to_string()).collect(), self.form.selection(108)), "Example word", [24.0, 602.0, content - 290.0, 34.0]),
                E::new(111, K::Label, "Keyboard layout", [width - 300.0, 578.0, 160.0, 24.0]),
                E::new(109, K::Choice(vec!["English keyboard".into(), "Hebrew keyboard".into()], self.form.selection(109)), "Preview keyboard layout", [width - 300.0, 602.0, 160.0, 34.0]),
                E::new(107, K::Button, "Preview", [width - 124.0, 602.0, 100.0, 34.0]),
                E::new(32, K::Text, &self.preview, [24.0, 650.0, content, 86.0]),
                E::new(33, K::Label, "Preview excludes prior words and application restrictions. Choose Preview again after changing settings.", [24.0, 744.0, content, 40.0]),
            ]);
        } else if self.page == 1 {
            scene.push(E::new(30, K::Label, "Undo only never converts an unchanged word. For immediate undo, choose a different Ctrl key from the double-tap action. Shared keys wait half a second. Existing bindings stay unchanged until you edit them.", [24.0, 476.0, content, 90.0]));
        }
        scene.push(E::new(
            90,
            K::Label,
            &self.message,
            [24.0, height - 48.0, content, 40.0],
        ));
        self.form.render_scrolling(scene)
    }
}
