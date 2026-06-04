use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;

use crate::config::Config;

/// Supported layout languages.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Language {
    English,
    Hebrew,
}

impl Language {
    /// Opposite layout.
    pub fn other(self) -> Self {
        match self {
            Language::English => Language::Hebrew,
            Language::Hebrew => Language::English,
        }
    }
}

/// Global runtime configuration, set once at startup.
static GLOBAL_CONFIG: OnceLock<Config> = OnceLock::new();

impl Config {
    /// Access the global config, falling back to defaults if not yet set.
    pub fn global() -> &'static Config {
        GLOBAL_CONFIG.get_or_init(|| Config {
            short_enabled: true,
            split_enabled: true,
        })
    }
}

/// Shared runtime state between the keyboard listener and the optional GUI.
pub struct AppControl {
    enabled: AtomicBool,
    fixed_count: AtomicU64,
}

impl AppControl {
    /// Create a new control and register the provided config globally.
    pub fn new_with_config(cfg: Config) -> Self {
        let _ = GLOBAL_CONFIG.set(cfg);
        Self {
            enabled: AtomicBool::new(true),
            fixed_count: AtomicU64::new(0),
        }
    }

    /// Legacy constructor – uses default (empty) config.
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
