#[derive(Clone, Debug)]
pub struct Config {
    /// Enable short‑word auto‑switch (≤3 chars).
    pub short_enabled: bool,
    /// Enable missing‑space split fallback.
    pub split_enabled: bool,
}

impl Config {
    /// Load configuration from environment variables.
    /// RECAST_SHORT  – any value enables short‑word shortcut.
    /// RECAST_SPLIT  – any value enables split fallback.
    pub fn from_env() -> Self {
        Self {
            short_enabled: std::env::var_os("RECAST_SHORT").is_some(),
            split_enabled: std::env::var_os("RECAST_SPLIT").is_some(),
        }
    }
}
