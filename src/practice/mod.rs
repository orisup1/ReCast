//! A live exercise in ReCast's own text field. The regular engine does the work.

use crate::types::{AppControl, FixKind};
use std::sync::atomic::Ordering;

pub fn first_run() -> bool {
    crate::complete::user_path("practice-offered").is_some_and(|p| !p.exists())
}

pub fn opened(control: &AppControl) {
    control.practice_stage.store(0, Ordering::Relaxed);
    control.practice_open.store(true, Ordering::Relaxed);
    if let Some(path) = crate::complete::user_path("practice-offered") {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
            let _ = std::fs::write(path, "Practice offered\n");
        }
    }
}

pub fn instructions() -> &'static str {
    "1. Select your English keyboard. In the field below, press Space, type akuo, then Space. Watch it become שלום.\n2. Immediately double-tap Ctrl to restore akuo.\n3. Select all and delete. Press Space, type keyb, then tap Right Shift to complete keyboard.\n\nThis is real correction in a local practice field. Practice does not save learned words or change your correction counts."
}

pub fn shortcut_label(value: &str) -> &'static str {
    match value {
        "left_ctrl" => "Also single-tap Left Ctrl",
        "right_ctrl" => "Also single-tap Right Ctrl",
        _ => "Double-tap Ctrl only",
    }
}

pub fn feedback(control: &AppControl) -> String {
    let stage = control.practice_stage.load(Ordering::Relaxed);
    let config = crate::config::Config::global();
    let step = match stage {
        0 if crate::complete::ignored("akuo") || crate::complete::learned("akuo") || crate::complete::suppressed("akuo") => "The practice word akuo is ignored. Allow it in your word lists before trying this exercise; practice will not change your exceptions.",
        0 => "Try step 1. If nothing changes, check the status below and enable both keyboards.",
        1 => "Correction worked. Double-tap Ctrl now to undo, without clicking or typing first.",
        2 if !config.complete_enabled => "Undo worked. Enable Word completion in Settings, then clear the field and continue step 3.",
        2 if config.complete_min_len > 4 => "Undo worked. Your minimum completion prefix is longer than keyb. Set complete_min to 4 or less and reopen ReCast to finish this exercise.",
        2 => "Undo worked. Clear the field, press Space, then try keyb + Right Shift.",
        _ => "You did it: correction, undo, and completion. Close this window and keep typing anywhere.",
    };
    format!("{step}\n{}\nOutside practice, one undo creates a session exception; two occasions save it across restarts.", shortcut_label(&config.undo_shortcut))
}

pub fn fixed(control: &AppControl, from: &str, to: &str, kind: FixKind) {
    let (old, next) = match (from, to, kind) {
        ("akuo", "שלום", FixKind::Layout) => (0, 1),
        ("keyb", "keyboard", FixKind::Complete) => (2, 3),
        _ => return,
    };
    let _ =
        control
            .practice_stage
            .compare_exchange(old, next, Ordering::Relaxed, Ordering::Relaxed);
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod native;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn practice_progress_requires_the_actual_operations_in_order() {
        let control = AppControl::new_for_test();
        fixed(&control, "keyb", "keyboard", FixKind::Complete);
        assert_eq!(control.practice_stage.load(Ordering::Relaxed), 0);
        fixed(&control, "akuo", "שלום", FixKind::Layout);
        assert_eq!(control.practice_stage.load(Ordering::Relaxed), 1);
        control.practice_stage.store(2, Ordering::Relaxed);
        fixed(&control, "keyb", "keyboard", FixKind::Complete);
        assert_eq!(control.practice_stage.load(Ordering::Relaxed), 3);
        assert_eq!(control.fixed_count(), 0);
    }
}
