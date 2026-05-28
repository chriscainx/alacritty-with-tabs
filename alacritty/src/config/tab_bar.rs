//! Tab bar configuration.

use serde::Serialize;

use alacritty_config_derive::ConfigDeserialize;

/// Configuration for the tab bar appearance.
#[derive(ConfigDeserialize, Serialize, Debug, Clone, PartialEq)]
pub struct TabBarConfig {
    /// Whether the tab bar is enabled.
    ///
    /// Default: `true`.
    pub enabled: bool,

    /// Height of the tab bar in pixels.
    ///
    /// Set to `0` for automatic height (2 × cell height).
    ///
    /// Default: `0`.
    pub height: u32,

    /// Whether to show the "+" new-tab button.
    ///
    /// Default: `true`.
    pub show_new_button: bool,

    /// Whether to show the "×" close button on each tab.
    ///
    /// Default: `true`.
    pub show_close_button: bool,
}

impl Default for TabBarConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            height: 0,
            show_new_button: true,
            show_close_button: true,
        }
    }
}
