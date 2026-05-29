# Multi-Tab Support

Alacritty supports multiple terminal sessions within a single window through a
tab bar rendered at the bottom of the terminal grid.

## Architecture

### Design

Each tab holds an independent terminal session: its own PTY, `Term` state, and
I/O event loop thread. All tabs share a single `Display` (window + renderer +
OpenGL context), avoiding the overhead of multiple GPU surfaces.

The active tab's state lives inline in `WindowContext`. Background tabs are
stored in a `Vec<Tab>`. Switching tabs swaps state via `mem::swap` — no
allocation, no PTY restart.

```
┌─────────────────────────────────────────────┐
│                 Window                       │
│  ┌───────────────────────────────────────┐  │
│  │          Terminal Grid                │  │
│  │          (active tab)                 │  │
│  │                                       │  │
│  ├───────────────────────────────────────┤  │
│  │ [Tab 1 ×] [Tab 2 ×] [Tab 3 ×]  [+]   │  │ ← TabBar
│  └───────────────────────────────────────┘  │
└─────────────────────────────────────────────┘
```

### Event Flow

Tab actions (create, close, switch) are routed through `EventType` variants to
the `Processor`, which dispatches them to `WindowContext` methods:

```
Keyboard / Mouse → input::Processor → Action
    → ActionContext::create_new_tab()  ──→ event_proxy.send(EventType::CreateTab)
    → ActionContext::close_tab()       ──→ event_proxy.send(EventType::CloseTab)
    → ActionContext::select_next_tab() ──→ event_proxy.send(EventType::SelectNextTab)
         ↓
    Processor::user_event()
         ↓
    WindowContext::create_new_tab() / close_active_tab() / switch_tab()
```

Mouse clicks on the tab bar are intercepted in `WindowContext::handle_event()`
before reaching the terminal input processor, so they never reach the PTY.

### Key Modules

| Module | Role |
|--------|------|
| `alacritty/src/tab.rs` | `Tab`, `TabBar`, `TabBarColors`, `TabBarHit` |
| `alacritty/src/window_context.rs` | Tab lifecycle, state swap, mouse intercept |
| `alacritty/src/display/mod.rs` | `draw_tab_bar_inner()`, `Display::tab_bar_lines` |
| `alacritty/src/config/tab_bar.rs` | `TabBarConfig` (TOML deserialization) |
| `alacritty/src/config/bindings.rs` | `CloseTab` action, platform keybindings |
| `alacritty/src/event.rs` | `EventType` variants, `Processor` handlers |
| `alacritty/src/input/mod.rs` | Non-macOS `Action` dispatch |

### Rendering

The tab bar occupies `tab_lines` grid rows at the **bottom** of the terminal
area, reserved via `SizeInfo::reserve_lines()`. This keeps the tab bar within
the normal grid coordinate system, avoiding projection-matrix tricks needed for
top-positioned bars.

1. **Rectangles** — drawn with `draw_rects()` at pixel coordinates computed from
   `size_info` (background, tab bodies, active indicator, "+" button).
2. **Text** — drawn with `draw_string()` at grid coordinates `(text_line, col)`,
   where `text_line = screen_lines + tab_lines - 1`.

All visual margins derive from `cell_height` and `cell_width` proportions,
ensuring they scale naturally with font size and HiDPI:

| Margin | Formula | ~px at 16px cell |
|--------|---------|-------------------|
| Tab border gap | `ch * 0.1` | 1.6 |
| Text horizontal padding | `cw * 0.25` | 4 |
| Active indicator height | `ch * 0.12` | 1.9 |
| Button internal padding | `ch * 0.25` | 4 |
| Tab min/max width | `cw * 5` / `cw * 15` | 80 / 240 |

## Configuration

Add a `[tab_bar]` section to `alacritty.toml`:

```toml
[tab_bar]
# Whether to show the tab bar (default: true).
enabled = true

# Height in pixels. Set to 0 for automatic (2 × cell height).
# Default: 0
height = 0

# Whether to show the "+" new-tab button (default: true).
show_new_button = true

# Whether to show the "×" close button on each tab (default: true).
show_close_button = true

# Max/min tab width in pixels. 0 = auto (15×/5× cell width).
max_tab_width = 0
min_tab_width = 0

# Double-click timeout for tab close (ms, default: 500).
double_click_timeout = 500

# Close window when the last tab is closed (default: true).
close_on_last_tab = true

# Predefined shells for the dropdown menu (▼ button).
[[tab_bar.shells]]
name = "PowerShell"
program = "pwsh.exe"

[[tab_bar.shells]]
name = "CMD"
program = "cmd.exe"

[[tab_bar.shells]]
name = "Git Bash"
program = "C:\\Program Files\\Git\\bin\\bash.exe"
args = ["-l"]
```

### Shell Dropdown

When `shells` is non-empty, a **▼** button appears to the left of the **+**
button. Clicking it opens a popup menu above the tab bar listing all
configured shells. Selecting a shell creates a new tab with that program.

The initial tab title is set to the shell's display name (e.g., "PowerShell").

Menu items are rendered at the same height as tabs, with thin separator lines
between entries. Clicking anywhere outside the menu dismisses it.

### Schema Compatibility

The `[tab_bar]` section is **additive** — it does not modify or conflict with
any existing configuration keys. All fields have defaults, so omitting the
section entirely preserves Alacritty's existing behavior.

| Scenario | Behavior |
|----------|----------|
| No `[tab_bar]` in config | All defaults: tab bar enabled, 2× cell height, both buttons shown |
| `[tab_bar]` present but partial | Missing fields use their defaults |
| Config serialized (e.g. `alacritty migrate`) | `[tab_bar]` is emitted with current values |
| Upstream config → this branch | `[tab_bar]` absent → uses defaults (no error) |
| **This branch's config → upstream Alacritty** | **Error**: `deny_unknown_fields` rejects the `tab_bar` key |

> ⚠️ Because `UiConfig` derives `#[serde(deny_unknown_fields)]`, a config file
> containing `[tab_bar]` **will not parse** on upstream Alacritty. This is a
> one-way incompatibility: branch configs must be stripped of `[tab_bar]`
> before being used with upstream builds.

**No existing config keys are affected.** The tab subsystem introduces only the
new `[tab_bar]` section with four fields:

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `true` | Show the tab bar |
| `height` | `u32` | `0` | Height in px (0 = auto: 2 × cell height) |
| `show_new_button` | `bool` | `true` | Show the "+" button |
| `show_close_button` | `bool` | `true` | Show "×" on each tab |

## Key Bindings

### Windows / Linux

| Shortcut | Action |
|----------|--------|
| `Ctrl` `Shift` `T` | Create new tab |
| `Ctrl` `Shift` `W` | Close current tab |
| `Ctrl` `Tab` | Next tab |
| `Ctrl` `Shift` `Tab` | Previous tab |
| `Ctrl` `1` – `8` | Switch to tab 1–8 |
| `Ctrl` `9` | Switch to last tab |

### macOS

macOS uses native `NSWindow` tabbing via the system tab bar (unchanged):

| Shortcut | Action |
|----------|--------|
| `Cmd` `T` | Create new tab |
| `Cmd` `Shift` `]` | Next tab |
| `Cmd` `Shift` `[` | Previous tab |
| `Cmd` `1` – `8` | Switch to tab 1–8 |
| `Cmd` `9` | Switch to last tab |

## Mouse Interaction

| Click target | Behavior |
|-------------|----------|
| Tab body | Switch to that tab |
| Close button (×) | Close that tab |
| "+" button | Create new tab |
| Last tab's × | Close the window |

## Public API

### `EventType` variants (cross-platform)

```rust
EventType::CreateTab           // Create a new tab in the current window
EventType::CloseTab            // Close the active tab
EventType::SelectNextTab       // Switch to the next tab
EventType::SelectPreviousTab   // Switch to the previous tab
EventType::SelectTab(usize)    // Switch to a specific tab by index
```

### `Action` variants

```rust
Action::CreateNewTab    // Create a new tab (non-macOS)
Action::CloseTab        // Close the current tab (non-macOS)
Action::SelectNextTab
Action::SelectPreviousTab
Action::SelectTab1 .. Action::SelectTab9
Action::SelectLastTab
```

### `WindowContext` methods

```rust
impl WindowContext {
    pub fn create_new_tab(&mut self, proxy: &EventLoopProxy<Event>) -> Result<(), Box<dyn Error>>;
    pub fn close_active_tab(&mut self) -> bool;  // false when last tab
    pub fn switch_tab(&mut self, index: usize);
    pub fn next_tab(&mut self);
    pub fn prev_tab(&mut self);
    pub fn tab_count(&self) -> usize;
    pub fn active_title(&self) -> &str;
    pub fn set_active_title(&mut self, title: String);
}
```

## Limitations

- **Text vertical centering**: `Point<usize>` cannot express fractional line
  numbers. With an even number of tab lines (e.g. 2), text sits in the lower
  line rather than being perfectly centered between both. An odd number of
  lines (1 or 3 via `tab_bar.height`) achieves natural centering.
- **Tab bar position**: Always at the bottom of the terminal grid. Top
  positioning is architecturally possible but not yet implemented.
