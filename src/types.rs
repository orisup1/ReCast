use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Supported layout languages.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Language {
    English,
    Hebrew,
}

impl Language {
    /// The opposite layout — handy for "switch to the other language" logic.
    pub fn other(self) -> Self {
        match self {
            Language::English => Language::Hebrew,
            Language::Hebrew => Language::English,
        }
    }
}

/// Shared runtime state between the keyboard listener and the optional GUI.
pub struct AppControl {
    enabled: AtomicBool,
    fixed_count: AtomicU64,
}

impl AppControl {
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(true),
            fixed_count: AtomicU64::new(0),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn set_enabled(&self, value: bool) {
        self.enabled.store(value, Ordering::Relaxed);
    }

    pub fn fixed_count(&self) -> u64 {
        self.fixed_count.load(Ordering::Relaxed)
    }

    pub fn record_fix(&self) {
        self.fixed_count.fetch_add(1, Ordering::Relaxed);
    }
}
