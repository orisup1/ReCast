//! A single guided step at a time, using the real keyboard engine.

use std::sync::atomic::Ordering;
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};

use crate::platform::desktop::{Element as E, Kind as K, Window as Form};
use crate::types::AppControl;

pub struct Window {
    pub form: Form,
}

impl Window {
    pub fn new(
        target: &EventLoopWindowTarget<()>,
        proxy: EventLoopProxy<()>,
        control: &AppControl,
    ) -> Result<Self, String> {
        let form = Form::new(
            target,
            proxy,
            "Practice ReCast",
            [720.0, 570.0],
            [660.0, 460.0],
        )?;
        let mut practice = Self { form };
        super::opened(control);
        practice.update(control, &crate::platform::status(control))?;
        practice.form.show();
        practice.focus();
        Ok(practice)
    }

    pub fn focus(&self) {
        self.form.focus(20);
    }

    /// Return true when the explicit Skip/Done button closes practice.
    pub fn update(&mut self, control: &AppControl, health: &str) -> Result<bool, String> {
        let mut refocus = false;
        for action in self.form.actions() {
            match action {
                10 => {
                    control.practice_stage.store(0, Ordering::Relaxed);
                    self.form.clear_input(20);
                    refocus = true;
                }
                11 => return Ok(true),
                #[cfg(target_os = "windows")]
                22 => unsafe {
                    use winapi::um::{shellapi::ShellExecuteW, winuser::SW_SHOWNORMAL};
                    let url: Vec<u16> = "ms-settings:regionlanguage"
                        .encode_utf16()
                        .chain(Some(0))
                        .collect();
                    let result = ShellExecuteW(
                        std::ptr::null_mut(),
                        std::ptr::null(),
                        url.as_ptr(),
                        std::ptr::null(),
                        std::ptr::null(),
                        SW_SHOWNORMAL,
                    ) as isize;
                    if result <= 32 {
                        return Err("Could not open Windows Language settings. Open Settings > Time & language > Language manually.".into());
                    }
                },
                _ => (),
            }
        }
        let [width, height] = self.form.size();
        let stage = control.practice_stage.load(Ordering::Relaxed);
        let (english, hebrew) = crate::layout::enabled_languages();
        let ready = english && hebrew;
        let state = |ready| if ready { "Ready" } else { "Needs setup" };
        let progress = if stage >= 3 {
            "Practice complete".into()
        } else {
            format!(
                "Step {} of 3 — {}",
                stage + 1,
                ["Correction", "Undo", "Completion"][stage as usize]
            )
        };
        #[allow(unused_mut)]
        let mut scene = vec![
            E::new(
                1,
                K::Heading,
                "Practice ReCast",
                [24.0, 20.0, width - 48.0, 30.0],
            ),
            E::new(
                2,
                K::Label,
                format!(
                    "English keyboard: {}   ·   Hebrew keyboard: {}",
                    state(english),
                    state(hebrew)
                ),
                [24.0, 62.0, width - 48.0, 36.0],
            ),
            E::new(3, K::Heading, progress, [24.0, 112.0, width - 48.0, 30.0]),
            E::new(
                4,
                K::Label,
                if ready {
                    super::step_instruction(stage)
                } else {
                    "Add the missing keyboards in your system's keyboard settings. Return here to continue; this checklist updates automatically.".into()
                },
                [24.0, 156.0, width - 48.0, 64.0],
            ),
            E::new(
                5,
                K::Label,
                "Practice typing field",
                [24.0, 226.0, width - 48.0, 24.0],
            ),
            E::new(
                20,
                K::Input,
                "Practice typing field",
                [24.0, 254.0, width - 48.0, 34.0],
            )
            .enabled(ready && stage < 3),
            E::new(
                6,
                K::Label,
                super::feedback(control).lines().next().unwrap_or_default(),
                [24.0, 306.0, width - 48.0, (height - 426.0).max(34.0)],
            ),
            E::new(
                7,
                K::Label,
                health,
                [24.0, height - 112.0, width - 48.0, 54.0],
            ),
            E::new(
                10,
                K::Button,
                "Start over",
                [24.0, height - 46.0, 116.0, 32.0],
            ),
            E::new(
                11,
                K::Button,
                if stage >= 3 { "Done" } else { "Skip practice" },
                [width - 154.0, height - 46.0, 130.0, 32.0],
            ),
        ];
        #[cfg(target_os = "windows")]
        scene.push(
            E::new(
                22,
                K::Button,
                "Keyboard settings…",
                [156.0, height - 46.0, 190.0, 32.0],
            )
            .enabled(!ready),
        );
        self.form.render(scene)?;
        if refocus {
            self.focus();
        }
        Ok(false)
    }
}
