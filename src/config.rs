#[derive(Clone, Debug)]
pub struct Config {
    /// Allow auto-switching on short words (≤3 chars). Short key sequences are
    /// dictionary-collision-prone, so this can be turned off for a stricter,
    /// never-wrongly-switch behaviour.
    pub short_enabled: bool,
    /// Enable missing‑space split fallback.
    pub split_enabled: bool,
}

impl Config {
    /// Load configuration from environment variables.
    /// RECAST_SHORT – set to `0` to disable switching on short (≤3 char) words
    ///                (default: enabled).
    /// RECAST_SPLIT – set (to anything but `0`) to enable the missing-space
    ///                split fallback (default: disabled).
    pub fn from_env() -> Self {
        Self {
            short_enabled: std::env::var("RECAST_SHORT")
                .map(|v| v != "0")
                .unwrap_or(true),
            split_enabled: std::env::var("RECAST_SPLIT")
                .map(|v| !v.is_empty() && v != "0")
                .unwrap_or(false),
        }
    }
}
