//! Tab management for multi-tab terminal windows.
//!
//! Each `Tab` holds an independent terminal session (PTY, Term state, notifier),
//! while the `TabBar` renders the visual tab strip and handles its hit-testing.

use std::sync::Arc;
use std::time::Instant;

use alacritty_terminal::event_loop::Notifier;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Term;

use crate::config::tab_bar::TabBarConfig;
use crate::display::color::Rgb;
use crate::display::SizeInfo;
use crate::event::{
    Event, EventProxy, InlineSearchState, Mouse, SearchState, TouchPurpose,
};

#[cfg(not(windows))]
use std::os::unix::io::RawFd;

/// Unique identifier for a tab within a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TabId(pub usize);

/// A single tab containing an independent terminal session.
///
/// When a tab is inactive (not the active tab in a WindowContext), all its
/// state lives here. When active, the state is swapped into WindowContext's
/// top-level fields.
pub struct Tab {
    pub id: TabId,
    pub title: String,
    pub terminal: Arc<FairMutex<Term<EventProxy>>>,
    pub notifier: Notifier,
    pub search_state: SearchState,
    pub inline_search_state: InlineSearchState,
    pub mouse: Mouse,
    pub touch: TouchPurpose,
    pub cursor_blink_timed_out: bool,
    pub prev_bell_cmd: Option<Instant>,
    pub occluded: bool,
    pub preserve_title: bool,
    pub event_queue: Vec<winit::event::Event<Event>>,
    #[cfg(not(windows))]
    pub master_fd: RawFd,
    #[cfg(not(windows))]
    pub shell_pid: u32,
}

/// Visual state of the tab bar, updated each frame before rendering.
pub struct TabBar {
    pub config: TabBarConfig,
    /// Cached titles for each tab (indexed by tab position).
    pub titles: Vec<String>,
    /// Which tab is active.
    pub active_index: usize,
    /// Pixel height of the tab bar (computed from config or font metrics).
    pub height_px: f32,
}

impl TabBar {
    pub fn new(config: TabBarConfig) -> Self {
        Self {
            config,
            titles: Vec::new(),
            active_index: 0,
            height_px: 0.0,
        }
    }

    /// Recompute the tab bar height from font metrics.
    ///
    /// If `config.height` is 0, defaults to `cell_height * 2`.
    pub fn compute_height(&mut self, cell_height: f32) {
        if self.config.height == 0 {
            self.height_px = cell_height * 2.0;
        } else {
            self.height_px = self.config.height as f32;
        }
    }

    /// Total number of tabs.
    pub fn len(&self) -> usize {
        self.titles.len()
    }

    /// Check whether a mouse position (in pixels, relative to window) hits the tab bar area.
    pub fn hit_test(&self, mouse_x: f32, mouse_y: f32, size_info: &SizeInfo) -> Option<TabBarHit> {
        if !self.config.enabled || self.titles.is_empty() {
            return None;
        }

        // Tab bar occupies reserved grid lines at the bottom of the terminal area.
        let tab_lines = (self.height_px / size_info.cell_height()).ceil() as usize;
        let bar_y = size_info.padding_y()
            + size_info.screen_lines() as f32 * size_info.cell_height();
        let bar_height = tab_lines as f32 * size_info.cell_height();

        if mouse_y < bar_y || mouse_y > bar_y + bar_height {
            return None;
        }

        let tab_count = self.titles.len();
        let new_button_width = if self.config.show_new_button {
            self.height_px
        } else {
            0.0
        };
        let available_width =
            size_info.width() - size_info.padding_x() * 2.0 - new_button_width;
        let tab_width = if tab_count > 0 {
            (available_width / tab_count as f32)
                .min(size_info.cell_width() * 15.0)
                .max(size_info.cell_width() * 5.0)
        } else {
            available_width
        };

        // "+" new tab button at the right edge.
        let new_btn_start = size_info.width() - size_info.padding_x() - new_button_width;
        if self.config.show_new_button && mouse_x >= new_btn_start {
            return Some(TabBarHit::NewButton);
        }

        let left = size_info.padding_x();
        for (i, _) in self.titles.iter().enumerate() {
            let tab_x = left + i as f32 * tab_width;
            let tab_right = (tab_x + tab_width).min(new_btn_start);

            if mouse_x >= tab_x && mouse_x < tab_right {
                // Close button: ~1.2 cells from the right edge, matches visual "×".
                if self.config.show_close_button
                    && mouse_x >= tab_right - size_info.cell_width() * 1.5
                {
                    return Some(TabBarHit::CloseButton(i));
                }
                return Some(TabBarHit::Tab(i));
            }
        }

        None
    }
}

/// Result of hit-testing a mouse position against the tab bar.
pub enum TabBarHit {
    /// Clicked on a tab at the given index.
    Tab(usize),
    /// Clicked on the close button of the tab at the given index.
    CloseButton(usize),
    /// Clicked on the "+" new-tab button.
    NewButton,
}

/// Color scheme for the tab bar, derived from the terminal theme.
pub struct TabBarColors {
    /// Background of the whole tab bar.
    pub bar_bg: Rgb,
    /// Background of an inactive tab.
    pub inactive_bg: Rgb,
    /// Background of the active tab.
    pub active_bg: Rgb,
    /// Text color for tab titles.
    pub text: Rgb,
    /// Text color for the active tab.
    pub active_text: Rgb,
    /// Color for the close button.
    pub close_button: Rgb,
}

impl TabBarColors {
    /// Derive tab bar colors from the terminal background color.
    pub fn from_background(bg: Rgb) -> Self {
        // Lighten/darken the background for various states.
        let lighten = |c: Rgb, amount: f32| -> Rgb {
            let r = (c.r as f32 + amount).clamp(0.0, 255.0) as u8;
            let g = (c.g as f32 + amount).clamp(0.0, 255.0) as u8;
            let b = (c.b as f32 + amount).clamp(0.0, 255.0) as u8;
            Rgb::new(r, g, b)
        };

        // Detect if background is dark or light.
        let luminance = 0.299 * bg.r as f32 + 0.587 * bg.g as f32 + 0.114 * bg.b as f32;
        let is_dark = luminance < 128.0;

        let (inactive_offset, text_brightness) = if is_dark {
            (25.0, 200)
        } else {
            (-25.0, 55)
        };

        let text_color = Rgb::new(text_brightness, text_brightness, text_brightness);
        let active_text = if is_dark {
            Rgb::new(255, 255, 255)
        } else {
            Rgb::new(0, 0, 0)
        };

        Self {
            bar_bg: lighten(bg, inactive_offset * 0.5),
            inactive_bg: lighten(bg, inactive_offset),
            active_bg: bg,
            text: text_color,
            active_text,
            close_button: Rgb::new(180, 60, 60),
        }
    }
}
