//! Small native forms shared by the macOS and Windows tray windows.
//!
//! Coordinates are logical pixels. Every render lays controls out again using
//! the current size and scale factor; native callbacks wake the existing Tao loop.

use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos::Native;
#[cfg(target_os = "windows")]
pub(super) mod windows;
#[cfg(target_os = "windows")]
use windows::Native;

#[derive(Clone, PartialEq)]
pub enum Kind {
    Label,
    Heading,
    Button,
    Check(bool),
    Choice(Vec<String>, usize),
    Input,
    Text,
}

impl Kind {
    fn same_control(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Choice(a, _), Self::Choice(b, _)) => a == b,
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }
}

#[derive(Clone)]
pub struct Element {
    pub id: u16,
    pub kind: Kind,
    pub text: String,
    pub frame: [f64; 4],
    pub enabled: bool,
    pub rtl: bool,
}

impl Element {
    pub fn new(id: u16, kind: Kind, text: impl Into<String>, frame: [f64; 4]) -> Self {
        Self {
            id,
            kind,
            text: text.into(),
            frame,
            enabled: true,
            rtl: false,
        }
    }

    pub fn directional(mut self) -> Self {
        self.rtl = self
            .text
            .chars()
            .find(|c| c.is_alphabetic())
            .is_some_and(|c| ('א'..='ת').contains(&c));
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

pub struct Window {
    // Tear down callbacks and native controls before dropping their Tao parent.
    native: Native,
    pub window: tao::window::Window,
}

impl Window {
    pub fn new(
        target: &EventLoopWindowTarget<()>,
        proxy: EventLoopProxy<()>,
        title: &str,
        size: [f64; 2],
        minimum: [f64; 2],
    ) -> Result<Self, String> {
        let size = target.primary_monitor().map_or(size, |monitor| {
            let screen = monitor.size().to_logical::<f64>(monitor.scale_factor());
            [
                size[0].min((screen.width - 64.0).max(minimum[0])),
                size[1].min((screen.height - 80.0).max(minimum[1])),
            ]
        });
        let window = tao::window::WindowBuilder::new()
            .with_title(title)
            .with_inner_size(tao::dpi::LogicalSize::new(size[0], size[1]))
            .with_min_inner_size(tao::dpi::LogicalSize::new(minimum[0], minimum[1]))
            .with_visible(false)
            .build(target)
            .map_err(|error| error.to_string())?;
        let native = Native::new(&window, proxy)?;
        Ok(Self { native, window })
    }

    pub fn size(&self) -> [f64; 2] {
        let size = self
            .window
            .inner_size()
            .to_logical::<f64>(self.window.scale_factor());
        [size.width, size.height]
    }

    pub fn render(&mut self, elements: Vec<Element>) -> Result<(), String> {
        self.native
            .render(&elements, self.size()[1], self.window.scale_factor(), false)
    }

    pub fn render_scrolling(&mut self, elements: Vec<Element>) -> Result<(), String> {
        self.native
            .render(&elements, self.size()[1], self.window.scale_factor(), true)
    }

    pub fn show(&self) {
        self.window.set_minimized(false);
        self.window.set_visible(true);
        self.window.set_focus();
    }

    pub fn focus(&self, id: u16) {
        self.window.set_focus();
        self.native.focus(id);
    }

    pub fn actions(&mut self) -> Vec<u16> {
        self.native.actions()
    }

    pub fn selection(&self, id: u16) -> usize {
        self.native.selection(id)
    }

    pub fn checked(&self, id: u16) -> bool {
        self.native.checked(id)
    }

    pub fn clear_input(&self, id: u16) {
        self.native.clear_input(id);
    }

    pub fn scroll_to_end(&self, id: u16, utf16_length: usize) {
        self.native.scroll_to_end(id, utf16_length);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn words_align_independently_of_punctuation() {
        assert!(
            Element::new(1, Kind::Label, "(שלום!)", [0.0; 4])
                .directional()
                .rtl
        );
        assert!(
            !Element::new(1, Kind::Label, "(hello!) שלום", [0.0; 4])
                .directional()
                .rtl
        );
        assert!(
            !Element::new(1, Kind::Label, "123", [0.0; 4])
                .directional()
                .rtl
        );
    }
}
