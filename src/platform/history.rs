//! A stable, bounded snapshot keeps an incoming correction from retargeting a click.

use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};

use super::desktop::{Element as E, Kind as K, Window as Form};
use crate::types::{AppControl, Correction};

const PAGE_SIZE: usize = 5;

pub struct Window {
    pub form: Form,
    corrections: Vec<Correction>,
    page: usize,
    page_size: usize,
    details: String,
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
            "ReCast — Recent corrections",
            [860.0, 650.0],
            [780.0, 460.0],
        )?;
        let mut window = Self {
            form,
            corrections: control.history(),
            page: 0,
            page_size: PAGE_SIZE,
            details: "Choose Details to inspect why a correction happened.".into(),
        };
        window.render()?;
        window.form.show();
        Ok(window)
    }

    pub fn update(&mut self, control: &AppControl) -> Result<(), String> {
        for action in self.form.actions() {
            match action {
                10 => {
                    self.corrections = control.history();
                    self.page = 0;
                    self.details = "Choose Details to inspect why a correction happened.".into();
                    break;
                }
                11 => {
                    self.page = self.page.saturating_sub(1);
                    break;
                }
                12 => {
                    if (self.page + 1) * self.page_size < self.corrections.len() {
                        self.page += 1;
                    }
                    break;
                }
                100..=149 => {
                    let slot = ((action - 100) / 10) as usize;
                    if let Some(correction) =
                        self.corrections.get(self.page * self.page_size + slot)
                    {
                        match action % 10 {
                            3 => self.details = correction.inspection(),
                            4 => {
                                crate::complete::ignore_word(&correction.from);
                            }
                            _ => (),
                        }
                    }
                }
                _ => (),
            }
        }
        self.render()
    }

    fn render(&mut self) -> Result<(), String> {
        let [width, height] = self.form.size();
        let page_size = (((height - 340.0) / 60.0).floor() as usize).clamp(2, PAGE_SIZE);
        let first = self.page * self.page_size;
        self.page_size = page_size;
        self.page = (first / page_size).min(self.corrections.len().saturating_sub(1) / page_size);
        let column = (width - 260.0) / 2.0;
        let mut scene = vec![
            E::new(
                1,
                K::Heading,
                "Recent corrections",
                [24.0, 20.0, width - 180.0, 30.0],
            ),
            E::new(
                2,
                K::Label,
                "Memory only. Refresh to include newer corrections.",
                [24.0, 58.0, width - 48.0, 28.0],
            ),
            E::new(10, K::Button, "Refresh", [width - 132.0, 20.0, 108.0, 32.0]),
            E::new(3, K::Label, "Original", [24.0, 106.0, column, 24.0]),
            E::new(
                4,
                K::Label,
                "Replacement",
                [40.0 + column, 106.0, column, 24.0],
            ),
        ];
        if self.corrections.is_empty() {
            scene.push(E::new(
                5,
                K::Label,
                "No corrections yet. Keep typing; your recent changes will appear here.",
                [24.0, 152.0, width - 48.0, 64.0],
            ));
        }
        for (slot, correction) in self
            .corrections
            .iter()
            .skip(self.page * self.page_size)
            .take(self.page_size)
            .enumerate()
        {
            let y = 130.0 + slot as f64 * 60.0;
            let id = 100 + slot as u16 * 10;
            let ignored = crate::complete::ignored(&correction.from);
            scene.extend([
                E::new(id, K::Label, &correction.from, [24.0, y, column, 25.0]),
                E::new(
                    id + 1,
                    K::Label,
                    &correction.to,
                    [40.0 + column, y, column, 25.0],
                ),
                E::new(
                    id + 2,
                    K::Label,
                    format!(
                        "{}  ·  {}  ·  {}",
                        correction.at.format("%H:%M:%S"),
                        correction.kind.tag(),
                        if correction.undone {
                            "Undone"
                        } else {
                            "Applied"
                        }
                    ),
                    [24.0, y + 28.0, width - 248.0, 24.0],
                ),
                E::new(id + 3, K::Button, "Details", [width - 208.0, y, 80.0, 32.0]),
                E::new(
                    id + 4,
                    K::Button,
                    if ignored { "Ignored" } else { "Ignore" },
                    [width - 116.0, y, 92.0, 32.0],
                )
                .enabled(!ignored),
            ]);
        }
        scene.extend([
            E::new(
                6,
                K::Label,
                "Correction details",
                [24.0, height - 210.0, width - 48.0, 24.0],
            ),
            E::new(
                7,
                K::Text,
                &self.details,
                [24.0, height - 182.0, width - 48.0, 120.0],
            ),
            E::new(
                11,
                K::Button,
                "Previous",
                [24.0, height - 46.0, 104.0, 32.0],
            )
            .enabled(self.page > 0),
            E::new(
                8,
                K::Label,
                format!(
                    "Page {} of {}  ·  {} corrections",
                    self.page + 1,
                    self.corrections.len().div_ceil(self.page_size).max(1),
                    self.corrections.len()
                ),
                [144.0, height - 42.0, width - 288.0, 26.0],
            ),
            E::new(
                12,
                K::Button,
                "Next",
                [width - 128.0, height - 46.0, 104.0, 32.0],
            )
            .enabled((self.page + 1) * self.page_size < self.corrections.len()),
        ]);
        self.form.render(scene)
    }
}
