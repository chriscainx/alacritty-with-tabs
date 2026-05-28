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
    /// Set to `0` for automatic height based on font size
    /// (`cell_height * 1.6`).
    ///
    /// Default: `0`.
    pub height: u32,

    /// Position of the tab bar: `"top"` or `"bottom"`.
    ///
    /// Default: `"top"`.
    pub position: TabBarPosition,

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
            position: TabBarPosition::Top,
            show_new_button: true,
            show_close_button: true,
        }
    }
}

/// Where the tab bar is placed relative to the terminal grid.
#[derive(ConfigDeserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabBarPosition {
    /// Tab bar at the top of the window.
    Top,
    /// Tab bar at the bottom of the window.
    Bottom,
}
