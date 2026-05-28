//! Terminal window context.

use std::error::Error;
use std::fs::File;
use std::io::Write;
use std::mem;
#[cfg(not(windows))]
use std::os::unix::io::{AsRawFd, RawFd};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use glutin::config::Config as GlutinConfig;
use glutin::display::GetGlDisplay;
#[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
use glutin::platform::x11::X11GlConfigExt;
use log::info;
use serde_json as json;
use winit::event::{ElementState, Event as WinitEvent, Modifiers, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::raw_window_handle::HasDisplayHandle;
use winit::window::WindowId;

use alacritty_terminal::event::Event as TerminalEvent;
use alacritty_terminal::event_loop::{EventLoop as PtyEventLoop, Msg, Notifier};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::Direction;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::NamedColor;

use crate::cli::{ParsedOptions, WindowOptions};
use crate::clipboard::Clipboard;
use crate::config::UiConfig;
use crate::display::Display;
use crate::display::window::Window;
use crate::event::{
    ActionContext, Event, EventProxy, InlineSearchState, Mouse, SearchState, TouchPurpose,
};
#[cfg(unix)]
use crate::logging::LOG_TARGET_IPC_CONFIG;
use crate::message_bar::MessageBuffer;
use crate::scheduler::Scheduler;
use crate::tab::{Tab, TabBar, TabBarHit, TabId};
use crate::{input, renderer};

/// Event context for one individual Alacritty window.
pub struct WindowContext {
    pub message_buffer: MessageBuffer,
    pub display: Display,
    pub dirty: bool,
    pub tab_bar: TabBar,

    /// Per-tab state for the currently active tab.
    event_queue: Vec<WinitEvent<Event>>,
    terminal: Arc<FairMutex<Term<EventProxy>>>,
    cursor_blink_timed_out: bool,
    prev_bell_cmd: Option<Instant>,
    modifiers: Modifiers,
    inline_search_state: InlineSearchState,
    search_state: SearchState,
    notifier: Notifier,
    mouse: Mouse,
    touch: TouchPurpose,
    occluded: bool,
    preserve_title: bool,
    active_title: String,
    #[cfg(not(windows))]
    master_fd: RawFd,
    #[cfg(not(windows))]
    shell_pid: u32,

    /// Inactive tabs (background sessions).
    inactive_tabs: Vec<Tab>,
    /// Index of the active tab in the logical tab list.
    active_tab_index: usize,
    next_tab_id: usize,

    /// Last known mouse cursor position in physical pixels.
    latest_mouse_pos: Option<(f64, f64)>,

    /// Event loop proxy for sending events back to the main loop.
    event_proxy: EventLoopProxy<Event>,

    window_config: ParsedOptions,
    config: Rc<UiConfig>,
}

impl WindowContext {
    /// Create initial window context that does bootstrapping the graphics API we're going to use.
    pub fn initial(
        event_loop: &ActiveEventLoop,
        proxy: EventLoopProxy<Event>,
        config: Rc<UiConfig>,
        mut options: WindowOptions,
    ) -> Result<Self, Box<dyn Error>> {
        let raw_display_handle = event_loop.display_handle().unwrap().as_raw();

        let mut identity = config.window.identity.clone();
        options.window_identity.override_identity_config(&mut identity);

        #[cfg(windows)]
        let window = Window::new(event_loop, &config, &identity, &mut options)?;
        #[cfg(windows)]
        let raw_window_handle = Some(window.raw_window_handle());

        #[cfg(not(windows))]
        let raw_window_handle = None;

        let gl_display = renderer::platform::create_gl_display(
            raw_display_handle,
            raw_window_handle,
            config.debug.prefer_egl,
        )?;
        let gl_config = renderer::platform::pick_gl_config(&gl_display, raw_window_handle)?;

        #[cfg(not(windows))]
        let window = Window::new(
            event_loop,
            &config,
            &identity,
            &mut options,
            #[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
            gl_config.x11_visual(),
        )?;

        let gl_context =
            renderer::platform::create_gl_context(&gl_display, &gl_config, raw_window_handle)?;

        let display = Display::new(window, gl_context, &config, false)?;

        Self::new(display, config, options, proxy)
    }

    /// Create additional context with the graphics platform other windows are using.
    pub fn additional(
        gl_config: &GlutinConfig,
        event_loop: &ActiveEventLoop,
        proxy: EventLoopProxy<Event>,
        config: Rc<UiConfig>,
        mut options: WindowOptions,
        config_overrides: ParsedOptions,
    ) -> Result<Self, Box<dyn Error>> {
        let gl_display = gl_config.display();

        let mut identity = config.window.identity.clone();
        options.window_identity.override_identity_config(&mut identity);

        #[cfg(target_os = "macos")]
        let tabbed = options.window_tabbing_id.is_some();
        #[cfg(not(target_os = "macos"))]
        let tabbed = false;

        let window = Window::new(
            event_loop,
            &config,
            &identity,
            &mut options,
            #[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
            gl_config.x11_visual(),
        )?;

        let raw_window_handle = window.raw_window_handle();
        let gl_context =
            renderer::platform::create_gl_context(&gl_display, gl_config, Some(raw_window_handle))?;

        let display = Display::new(window, gl_context, &config, tabbed)?;

        let mut window_context = Self::new(display, config, options, proxy)?;
        window_context.window_config = config_overrides;

        Ok(window_context)
    }

    /// Build the per-tab components (Term, PTY, event loop) for a new session.
    fn build_tab_session(
        display: &Display,
        config: &UiConfig,
        options: &WindowOptions,
        proxy: &EventLoopProxy<Event>,
        tab_id: TabId,
    ) -> Result<Tab, Box<dyn Error>> {
        let mut pty_config = config.pty_config();
        options.terminal_options.override_pty_config(&mut pty_config);

        let preserve_title = options.window_identity.title.is_some();

        let event_proxy = EventProxy::new(proxy.clone(), display.window.id());

        let terminal =
            Term::new(config.term_options(), &display.size_info, event_proxy.clone());
        let terminal = Arc::new(FairMutex::new(terminal));

        let pty =
            tty::new(&pty_config, display.size_info.into(), display.window.id().into())?;

        #[cfg(not(windows))]
        let master_fd = pty.file().as_raw_fd();
        #[cfg(not(windows))]
        let shell_pid = pty.child().id();

        let event_loop = PtyEventLoop::new(
            Arc::clone(&terminal),
            event_proxy.clone(),
            pty,
            pty_config.drain_on_exit,
            config.debug.ref_test,
        )?;

        let loop_tx = event_loop.channel();

        // Spawn the PTY I/O thread (held alive by the event loop internals).
        let _io_thread = event_loop.spawn();

        if config.cursor.style().blinking {
            event_proxy.send_event(TerminalEvent::CursorBlinkingChange.into());
        }

        let title = config.window.identity.title.clone();

        Ok(Tab {
            id: tab_id,
            title,
            terminal,
            notifier: Notifier(loop_tx),
            search_state: SearchState::default(),
            inline_search_state: InlineSearchState::default(),
            mouse: Mouse::default(),
            touch: TouchPurpose::None,
            cursor_blink_timed_out: false,
            prev_bell_cmd: None,
            occluded: false,
            preserve_title,
            event_queue: Vec::new(),
            #[cfg(not(windows))]
            master_fd,
            #[cfg(not(windows))]
            shell_pid,
        })
    }

    /// Create a new terminal window context.
    fn new(
        mut display: Display,
        config: Rc<UiConfig>,
        options: WindowOptions,
        proxy: EventLoopProxy<Event>,
    ) -> Result<Self, Box<dyn Error>> {
        // Compute tab bar height and reserve grid lines before creating the PTY.
        let tab_bar_height = (display.size_info.cell_height() * 1.6).ceil();
        let tab_bar_lines =
            (tab_bar_height / display.size_info.cell_height()).ceil() as usize;
        display.tab_bar_height = tab_bar_height;
        display.tab_bar_lines = tab_bar_lines;
        display.size_info.reserve_lines(tab_bar_lines);

        info!(
            "PTY dimensions: {:?} x {:?}",
            display.size_info.screen_lines(),
            display.size_info.columns()
        );

        let first_tab = Self::build_tab_session(
            &display,
            &config,
            &options,
            &proxy,
            TabId(0),
        )?;

        let mut tab_bar = TabBar::new(config.tab_bar.clone());
        tab_bar.titles.push(first_tab.title.clone());
        tab_bar.compute_height(display.size_info.cell_height());

        let preserve_title = first_tab.preserve_title;
        let title = first_tab.title;

        Ok(WindowContext {
            preserve_title,
            active_title: title,
            terminal: first_tab.terminal,
            notifier: first_tab.notifier,
            search_state: first_tab.search_state,
            inline_search_state: first_tab.inline_search_state,
            mouse: first_tab.mouse,
            touch: first_tab.touch,
            cursor_blink_timed_out: first_tab.cursor_blink_timed_out,
            prev_bell_cmd: first_tab.prev_bell_cmd,
            occluded: first_tab.occluded,
            event_queue: first_tab.event_queue,
            #[cfg(not(windows))]
            master_fd: first_tab.master_fd,
            #[cfg(not(windows))]
            shell_pid: first_tab.shell_pid,
            display,
            config,
            tab_bar,
            inactive_tabs: Vec::new(),
            active_tab_index: 0,
            next_tab_id: 1,
            latest_mouse_pos: None,
            event_proxy: proxy,
            message_buffer: Default::default(),
            window_config: Default::default(),
            modifiers: Default::default(),
            dirty: true,
        })
    }

    // ── Tab management ────────────────────────────────────────────

    /// Create a new tab in this window.
    pub fn create_new_tab(&mut self, proxy: &EventLoopProxy<Event>) -> Result<(), Box<dyn Error>> {
        let tab_id = TabId(self.next_tab_id);
        self.next_tab_id += 1;

        let options = WindowOptions::default();
        let new_tab = Self::build_tab_session(
            &self.display,
            &self.config,
            &options,
            proxy,
            tab_id,
        )?;

        let new_title = new_tab.title.clone();
        let target_index = self.tab_bar.titles.len();

        self.tab_bar.titles.push(new_title);
        self.inactive_tabs.push(new_tab);
        self.switch_tab(target_index);
        self.dirty = true;

        Ok(())
    }

    /// Close the currently active tab.
    ///
    /// Returns `true` if the tab was closed, `false` if it cannot be closed
    /// (last remaining tab).
    pub fn close_active_tab(&mut self) -> bool {
        if self.inactive_tabs.is_empty() && self.tab_bar.titles.len() <= 1 {
            return false;
        }

        let _ = self.notifier.0.send(Msg::Shutdown);

        if let Some(incoming) = self.inactive_tabs.pop() {
            let removed_index = self.active_tab_index;
            self.swap_active_with(incoming);
            self.tab_bar.titles.remove(removed_index);
            if self.active_tab_index >= self.tab_bar.titles.len() {
                self.active_tab_index = self.tab_bar.titles.len().saturating_sub(1);
            }
            self.tab_bar.active_index = self.active_tab_index;
        }

        self.dirty = true;
        true
    }

    /// Swap the active tab's state with an incoming `Tab`, shutting down the old.
    fn swap_active_with(&mut self, mut incoming: Tab) {
        mem::swap(&mut self.terminal, &mut incoming.terminal);
        mem::swap(&mut self.notifier, &mut incoming.notifier);
        mem::swap(&mut self.search_state, &mut incoming.search_state);
        mem::swap(&mut self.inline_search_state, &mut incoming.inline_search_state);
        mem::swap(&mut self.mouse, &mut incoming.mouse);
        mem::swap(&mut self.touch, &mut incoming.touch);
        mem::swap(&mut self.cursor_blink_timed_out, &mut incoming.cursor_blink_timed_out);
        mem::swap(&mut self.prev_bell_cmd, &mut incoming.prev_bell_cmd);
        mem::swap(&mut self.occluded, &mut incoming.occluded);
        mem::swap(&mut self.preserve_title, &mut incoming.preserve_title);
        mem::swap(&mut self.event_queue, &mut incoming.event_queue);
        mem::swap(&mut self.active_title, &mut incoming.title);
        #[cfg(not(windows))]
        {
            mem::swap(&mut self.master_fd, &mut incoming.master_fd);
            mem::swap(&mut self.shell_pid, &mut incoming.shell_pid);
        }

        self.display.window.set_title(self.active_title.clone());

        let _ = incoming.notifier.0.send(Msg::Shutdown);
        drop(incoming);
    }

    /// Switch to the tab at the given logical index.
    pub fn switch_tab(&mut self, index: usize) {
        let total = self.tab_bar.titles.len();
        if index >= total || index == self.active_tab_index {
            return;
        }

        let vec_index = if index > self.active_tab_index {
            index - 1
        } else {
            index
        };

        self.tab_bar.titles.swap(self.active_tab_index, index);

        let mut target = self.inactive_tabs.remove(vec_index);
        mem::swap(&mut self.terminal, &mut target.terminal);
        mem::swap(&mut self.notifier, &mut target.notifier);
        mem::swap(&mut self.search_state, &mut target.search_state);
        mem::swap(&mut self.inline_search_state, &mut target.inline_search_state);
        mem::swap(&mut self.mouse, &mut target.mouse);
        mem::swap(&mut self.touch, &mut target.touch);
        mem::swap(&mut self.cursor_blink_timed_out, &mut target.cursor_blink_timed_out);
        mem::swap(&mut self.prev_bell_cmd, &mut target.prev_bell_cmd);
        mem::swap(&mut self.occluded, &mut target.occluded);
        mem::swap(&mut self.preserve_title, &mut target.preserve_title);
        mem::swap(&mut self.event_queue, &mut target.event_queue);
        mem::swap(&mut self.active_title, &mut target.title);
        #[cfg(not(windows))]
        {
            mem::swap(&mut self.master_fd, &mut target.master_fd);
            mem::swap(&mut self.shell_pid, &mut target.shell_pid);
        }

        self.display.window.set_title(self.active_title.clone());

        self.inactive_tabs.insert(vec_index, target);

        self.active_tab_index = index;
        self.tab_bar.active_index = index;
        self.dirty = true;
    }

    /// Switch to the next tab (wraps around).
    pub fn next_tab(&mut self) {
        let total = self.tab_bar.titles.len();
        if total > 1 {
            let next = (self.active_tab_index + 1) % total;
            self.switch_tab(next);
        }
    }

    /// Switch to the previous tab (wraps around).
    pub fn prev_tab(&mut self) {
        let total = self.tab_bar.titles.len();
        if total > 1 {
            let prev = if self.active_tab_index == 0 {
                total - 1
            } else {
                self.active_tab_index - 1
            };
            self.switch_tab(prev);
        }
    }

    pub fn active_title(&self) -> &str {
        &self.active_title
    }

    pub fn tab_count(&self) -> usize {
        self.tab_bar.titles.len()
    }

    pub fn set_active_title(&mut self, title: String) {
        self.active_title = title.clone();
        self.tab_bar.titles[self.active_tab_index] = title;
    }

    pub fn tab_bar(&self) -> &TabBar {
        &self.tab_bar
    }

    pub fn tab_bar_mut(&mut self) -> &mut TabBar {
        &mut self.tab_bar
    }

    /// Handle a click on the tab bar.
    fn handle_tab_bar_click(&mut self, hit: TabBarHit) {
        match hit {
            TabBarHit::Tab(index) => self.switch_tab(index),
            TabBarHit::CloseButton(index) => {
                if index == self.active_tab_index {
                    self.close_active_tab();
                } else {
                    // Close a background tab.
                    let vec_index = if index > self.active_tab_index {
                        index - 1
                    } else {
                        index
                    };
                    if vec_index < self.inactive_tabs.len() {
                        let tab = self.inactive_tabs.remove(vec_index);
                        let _ = tab.notifier.0.send(
                            alacritty_terminal::event_loop::Msg::Shutdown,
                        );
                        self.tab_bar.titles.remove(index);
                        if self.active_tab_index > index {
                            self.active_tab_index -= 1;
                        }
                        self.tab_bar.active_index = self.active_tab_index;
                        self.dirty = true;
                    }
                }
            },
            TabBarHit::NewButton => {
                let _ = self.event_proxy.send_event(Event::new(
                    crate::event::EventType::CreateTab,
                    self.display.window.id(),
                ));
            },
        }
    }

    // ── Config & rendering ────────────────────────────────────────

    pub fn update_config(&mut self, new_config: Rc<UiConfig>) {
        let old_config = mem::replace(&mut self.config, new_config);

        self.config = self.window_config.override_config_rc(self.config.clone());

        self.display.update_config(&self.config);
        self.terminal.lock().set_options(self.config.term_options());

        for tab in &self.inactive_tabs {
            tab.terminal.lock().set_options(self.config.term_options());
        }

        if (old_config.cursor.thickness() - self.config.cursor.thickness()).abs() > f32::EPSILON {
            self.display.pending_update.set_cursor_dirty();
        }

        if old_config.font != self.config.font {
            let scale_factor = self.display.window.scale_factor as f32;
            if self.display.font_size == old_config.font.size().scale(scale_factor) {
                self.display.font_size = self.config.font.size().scale(scale_factor);
            }
            let font = self.config.font.clone().with_size(self.display.font_size);
            self.display.pending_update.set_font(font);
        }

        self.display.window.set_theme(self.config.window.theme());

        let window_config = &old_config.window;
        if window_config.padding(1.) != self.config.window.padding(1.)
            || window_config.dynamic_padding != self.config.window.dynamic_padding
            || window_config.resize_increments != self.config.window.resize_increments
        {
            self.display.pending_update.dirty = true;
        }

        if !self.preserve_title
            && (!self.config.window.dynamic_title
                || self.display.window.title() == old_config.window.identity.title)
        {
            self.display.window.set_title(self.config.window.identity.title.clone());
            self.active_title = self.config.window.identity.title.clone();
        }

        let opaque = self.config.window_opacity() >= 1.;

        #[cfg(target_os = "macos")]
        self.display.window.set_has_shadow(opaque);

        #[cfg(target_os = "macos")]
        self.display.window.set_option_as_alt(self.config.window.option_as_alt());

        self.display.window.set_transparent(!opaque);
        self.display.window.set_blur(self.config.window.blur);

        self.display.hint_state.update_alphabet(self.config.hints.alphabet());

        let event = Event::new(TerminalEvent::CursorBlinkingChange.into(), None);
        self.event_queue.push(event.into());

        self.tab_bar.compute_height(self.display.size_info.cell_height());

        self.dirty = true;
    }

    #[cfg(unix)]
    pub fn config(&self) -> &UiConfig {
        &self.config
    }

    #[cfg(unix)]
    pub fn reset_window_config(&mut self, config: Rc<UiConfig>) {
        self.message_buffer.remove_target(LOG_TARGET_IPC_CONFIG);
        self.window_config.clear();
        self.update_config(config);
    }

    #[cfg(unix)]
    pub fn add_window_config(&mut self, config: Rc<UiConfig>, options: &ParsedOptions) {
        self.message_buffer.remove_target(LOG_TARGET_IPC_CONFIG);
        self.window_config.extend_from_slice(options);
        self.update_config(config);
    }

    pub fn draw(&mut self, scheduler: &mut Scheduler) {
        self.display.window.requested_redraw = false;

        if self.occluded {
            return;
        }

        self.dirty = false;

        self.display.process_renderer_update();

        if !self.display.visual_bell.completed() {
            if self.display.window.has_frame {
                self.display.window.request_redraw();
            } else {
                self.dirty = true;
            }
        }

        let terminal = self.terminal.lock();
        let bg = self.display.colors[NamedColor::Background as usize];
        let tab_colors = crate::tab::TabBarColors::from_background(bg);
        self.display.draw(
            terminal,
            scheduler,
            &self.message_buffer,
            &self.config,
            &mut self.search_state,
            Some(&self.tab_bar),
            Some(&tab_colors),
        );
    }

    pub fn handle_event(
        &mut self,
        #[cfg(target_os = "macos")] event_loop: &ActiveEventLoop,
        event_proxy: &EventLoopProxy<Event>,
        clipboard: &mut Clipboard,
        scheduler: &mut Scheduler,
        event: WinitEvent<Event>,
    ) {
        match event {
            WinitEvent::AboutToWait
            | WinitEvent::WindowEvent { event: WindowEvent::RedrawRequested, .. } => {
                if self.event_queue.is_empty() {
                    return;
                }
            },
            // Track mouse position for tab bar hit testing.
            WinitEvent::WindowEvent {
                event: WindowEvent::CursorMoved { device_id, position },
                window_id,
            } => {
                self.latest_mouse_pos = Some((position.x, position.y));
                self.event_queue.push(WinitEvent::WindowEvent {
                    event: WindowEvent::CursorMoved { device_id, position },
                    window_id,
                });
                return;
            },
            // Intercept mouse clicks in the tab bar area.
            WinitEvent::WindowEvent {
                event:
                    WindowEvent::MouseInput {
                        device_id,
                        state: state @ ElementState::Pressed,
                        button: button @ MouseButton::Left,
                    },
                window_id,
            } => {
                if let Some((x, y)) = self.latest_mouse_pos {
                    if let Some(hit) = self.tab_bar.hit_test(
                        x as f32,
                        y as f32,
                        &self.display.size_info,
                    ) {
                        self.handle_tab_bar_click(hit);
                        return;
                    }
                }
                self.event_queue.push(WinitEvent::WindowEvent {
                    event: WindowEvent::MouseInput { device_id, state, button },
                    window_id,
                });
                return;
            },
            event => {
                self.event_queue.push(event);
                return;
            },
        }

        let mut terminal = self.terminal.lock();

        let old_is_searching = self.search_state.history_index.is_some();

        let context = ActionContext {
            cursor_blink_timed_out: &mut self.cursor_blink_timed_out,
            prev_bell_cmd: &mut self.prev_bell_cmd,
            message_buffer: &mut self.message_buffer,
            inline_search_state: &mut self.inline_search_state,
            search_state: &mut self.search_state,
            modifiers: &mut self.modifiers,
            notifier: &mut self.notifier,
            display: &mut self.display,
            mouse: &mut self.mouse,
            touch: &mut self.touch,
            dirty: &mut self.dirty,
            occluded: &mut self.occluded,
            terminal: &mut terminal,
            #[cfg(not(windows))]
            master_fd: self.master_fd,
            #[cfg(not(windows))]
            shell_pid: self.shell_pid,
            preserve_title: self.preserve_title,
            config: &self.config,
            event_proxy,
            #[cfg(target_os = "macos")]
            event_loop,
            clipboard,
            scheduler,
        };
        let mut processor = input::Processor::new(context);

        for event in self.event_queue.drain(..) {
            processor.handle_event(event);
        }

        if self.display.pending_update.dirty {
            Self::submit_display_update(
                &mut terminal,
                &mut self.display,
                &mut self.notifier,
                &self.message_buffer,
                &mut self.search_state,
                old_is_searching,
                &self.config,
            );
            self.dirty = true;
        }

        if self.dirty || self.mouse.hint_highlight_dirty {
            self.dirty |= self.display.update_highlighted_hints(
                &terminal,
                &self.config,
                &self.mouse,
                self.modifiers.state(),
            );
            self.mouse.hint_highlight_dirty = false;
        }

        // Sync window title back to the active tab.
        let current_title = self.display.window.title().to_owned();
        if current_title != self.active_title {
            self.tab_bar.titles[self.active_tab_index] = current_title.clone();
            self.active_title = current_title;
        }

        if self.dirty
            && self.display.window.has_frame
            && !self.occluded
            && !matches!(event, WinitEvent::WindowEvent { event: WindowEvent::RedrawRequested, .. })
        {
            self.display.window.request_redraw();
        }
    }

    pub fn id(&self) -> WindowId {
        self.display.window.id()
    }

    pub fn write_ref_test_results(&self) {
        let mut grid = self.terminal.lock().grid().clone();
        grid.initialize_all();
        grid.truncate();

        let serialized_grid = json::to_string(&grid).expect("serialize grid");

        let size_info = &self.display.size_info;
        let size = TermSize::new(size_info.columns(), size_info.screen_lines());
        let serialized_size = json::to_string(&size).expect("serialize size");

        let serialized_config = format!("{{\"history_size\":{}}}", grid.history_size());

        File::create("./grid.json")
            .and_then(|mut f| f.write_all(serialized_grid.as_bytes()))
            .expect("write grid.json");

        File::create("./size.json")
            .and_then(|mut f| f.write_all(serialized_size.as_bytes()))
            .expect("write size.json");

        File::create("./config.json")
            .and_then(|mut f| f.write_all(serialized_config.as_bytes()))
            .expect("write config.json");
    }

    fn submit_display_update(
        terminal: &mut Term<EventProxy>,
        display: &mut Display,
        notifier: &mut Notifier,
        message_buffer: &MessageBuffer,
        search_state: &mut SearchState,
        old_is_searching: bool,
        config: &UiConfig,
    ) {
        let num_lines = terminal.screen_lines();
        let cursor_at_bottom = terminal.grid().cursor.point.line + 1 == num_lines;
        let origin_at_bottom = if terminal.mode().contains(TermMode::VI) {
            terminal.vi_mode_cursor.point.line == num_lines - 1
        } else {
            search_state.direction == Direction::Left
        };

        display.handle_update(terminal, notifier, message_buffer, search_state, config);

        let new_is_searching = search_state.history_index.is_some();
        if !old_is_searching && new_is_searching {
            let display_offset = terminal.grid().display_offset();
            if display_offset == 0 && cursor_at_bottom && !origin_at_bottom {
                terminal.scroll_display(Scroll::Delta(1));
            } else if display_offset != 0 && origin_at_bottom {
                terminal.scroll_display(Scroll::Delta(-1));
            }
        }
    }
}

impl Drop for WindowContext {
    fn drop(&mut self) {
        let _ = self.notifier.0.send(Msg::Shutdown);
        for tab in &self.inactive_tabs {
            let _ = tab.notifier.0.send(Msg::Shutdown);
        }
    }
}
