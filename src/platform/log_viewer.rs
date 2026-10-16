//! Read-only live log hosted by the tray loop, with native, scalable controls.

use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};

use super::desktop::{Element, Kind, Window as Form};
use crate::types::LiveLog;

pub struct Window {
    pub form: Form,
    last_text: String,
}

impl Window {
    pub fn new(
        target: &EventLoopWindowTarget<()>,
        proxy: EventLoopProxy<()>,
    ) -> Result<Self, String> {
        let form = Form::new(
            target,
            proxy,
            "ReCast live log",
            [760.0, 460.0],
            [600.0, 360.0],
        )?;
        let mut window = Self {
            form,
            last_text: String::new(),
        };
        window.render("")?;
        window.form.show();
        Ok(window)
    }

    pub fn focus(&self) {
        self.form.show();
    }

    pub fn update(&mut self, log: &LiveLog) -> Result<(), String> {
        self.form.window.set_title(if log.active() {
            "ReCast live log — recording"
        } else {
            "ReCast live log — stopped"
        });
        let text = log.text();
        self.render(&text)?;
        if text != self.last_text {
            self.form.scroll_to_end(2, text.encode_utf16().count());
            self.last_text = text;
        }
        Ok(())
    }

    fn render(&mut self, text: &str) -> Result<(), String> {
        let [width, height] = self.form.size();
        self.form.render(vec![
            Element::new(
                1,
                Kind::Label,
                "Start/stop from Help in the ReCast menu. Closing stops logging. Memory only.",
                [16.0, 12.0, width - 32.0, 40.0],
            ),
            Element::new(
                2,
                Kind::Text,
                text,
                [16.0, 60.0, width - 32.0, height - 76.0],
            ),
        ])
    }
}
