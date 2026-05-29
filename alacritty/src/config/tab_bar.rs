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

    /// Maximum width of a single tab in pixels.
    ///
    /// Set to `0` for automatic (15 × cell width).
    ///
    /// Default: `0`.
    pub max_tab_width: u32,

    /// Minimum width of a single tab in pixels.
    ///
    /// Set to `0` for automatic (5 × cell width).
    ///
    /// Default: `0`.
    pub min_tab_width: u32,

    /// Timeout in milliseconds for double-click tab close detection.
    ///
    /// Default: `500`.
    pub double_click_timeout: u32,

    /// Whether closing the last tab should close the entire window.
    ///
    /// If `false`, the last tab cannot be closed.
    ///
    /// Default: `true`.
    pub close_on_last_tab: bool,
}

impl Default for TabBarConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            height: 0,
            show_new_button: true,
            show_close_button: true,
            max_tab_width: 0,
            min_tab_width: 0,
            double_click_timeout: 500,
            close_on_last_tab: true,
        }
    }
}
