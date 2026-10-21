# deadbeef

A Hyprland-inspired tiling window manager for Windows 10/11, written in Rust.

deadbeef runs in the background on top of the normal Windows desktop. It
does not replace the compositor or fake anything: every operation is a real
Win32 call — `SetWinEventHook` to observe windows, `SetWindowPos` to tile,
`SetForegroundWindow` to focus, virtual-desktop COM to switch workspaces,
`DwmSetWindowAttribute` / Direct2D for borders.

```
deadbeef.exe   the window manager (tray icon, no console)
dbctl.exe      command-line control over \\.\pipe\deadbeef
```

## Build

Requires the Rust MSVC toolchain and the Windows SDK (for `rc.exe`).

```powershell
cargo build --release
# target\release\deadbeef.exe, target\release\dbctl.exe
cargo test
```

Release builds use fat LTO, `panic = "abort"` and a statically linked C
runtime (`.cargo/config.toml`). The binaries import only Windows system DLLs.

## Run

Start `deadbeef.exe`. The first time, it opens **deadbeef Settings** (below)
instead of tiling straight away: pick a look, check the preview, press Apply.

```
deadbeef.exe               first run / after a reset / already tiling → Settings, otherwise tile
deadbeef.exe --settings    the settings app (tray: Settings…, or double-click the tray icon)
deadbeef.exe --wm          tile (what start-up runs)
deadbeef.exe --install     install this build, then open Settings (doesn't start tiling)
deadbeef.exe --uninstall   what Apps & features runs
```

The profile lives in `%APPDATA%\deadbeef\`: the bundled template is installed to
`templates\nerdy-deadbeef\` (read-only) and copied to `profiles\active\` —
**the only place you edit**. Saving any file in the active profile reloads it immediately.

Tray menu: Settings… · Reload Config · Open Profile Folder · Open Log · Switch Profile ·
Reset to nerdy-deadbeef · Quit. Log: `%APPDATA%\deadbeef\deadbeef.log`.

## Settings app

A window for setting deadbeef up without editing files. The left side has
the options (Overview, Layout, Look, Top bar, Behaviour, Startup & reset); the
right side is a live **preview** of your desktop: the real dwindle/master code
laid out on your monitor's real size, with the bar, borders, corners, opacity
and gaps you picked. It also lists every pending change (old → new). Add or
remove preview windows and click one to see it focused.

Nothing changes until **Apply**, which:

1. writes `profiles\active\settings.conf` (only the options the app manages) and
   appends `source = settings.conf` to `deadbeef.conf` once, so these values win
   over `style.conf`. Your own files are otherwise left alone, and deleting
   `settings.conf` drops every change made in the app;
2. installs `deadbeef.exe` (+ `dbctl.exe`) to `%LOCALAPPDATA%\Programs\deadbeef\`,
   with a Start menu shortcut and an Apps & features entry;
3. registers `HKCU\…\Run\deadbeef = "…\deadbeef.exe" --wm`, so tiling comes back
   after a restart with the same settings (switch this off under Startup & reset);
4. starts the window manager, or reloads it if it is already running.

**Reset everything** (Startup & reset, click twice to confirm) stops tiling (which
restores every window and the Windows taskbar), removes the start-up entry and
resets the profile to the template. A copy of the old profile goes to `backups\`.
**Uninstall** does the same and also removes the installed files, the shortcut
and the Apps & features entry; `%APPDATA%\deadbeef` is kept.

## Profiles

```
%APPDATA%\deadbeef\
  templates\nerdy-deadbeef\   read-only, shipped inside the binary
  profiles\active\            your live config (watched)
  profiles\<name>\            snapshots
  backups\                    last 10 copies of `active`, taken before apply/switch
```

```powershell
dbctl profile list                    # templates + saved profiles (* = origin of active)
dbctl profile apply nerdy-deadbeef    # reset active to the template
dbctl profile save my-setup           # snapshot active
dbctl profile switch my-setup         # replace active with a snapshot
```

## Config

Hyprland-style grammar, Windows-native directives (`deadbeef.conf`):

```ini
$terminal = wt
source = %APPDATA%\deadbeef\profiles\active\style.conf
exec-once = yasb
bind = SUPER, X, exec, $terminal
windowrule = float, exe:^(calc\.exe)$
monitor = 0, preferred, 0x0, 1
workspace = 4, monitor:1

general {
    gaps_in = 5
    col.active_border = rgba(88c0d0ff) rgba(81a1c1ff) 45deg
}
```

- `#` starts a comment at line start or after whitespace (`class:^(#32770)$` works); `##` is a literal `#`.
- Paths use `%ENV%` expansion; relative `source`/`script` paths resolve against the profile.
- Monitors are numbered: `0` is the primary, the rest left-to-right (`dbctl monitors`).
- Window rule matchers: `exe:` (case-insensitive), `class:`, `title:` — all regex, all must match.
  Actions: `float tile center pin fullscreen maximize noborder ignore`,
  `workspace N [silent]`, `monitor N`, `opacity A [B]`, `size W H`, `move X Y` (px or %).
- Linux-only directives (`kb_layout`, `touchpad {}`, `desc:` monitors, Wayland rules) are rejected with an error.

Errors are reported with file:line in the log, `dbctl errors`, and a tray notification.

### Options

| key | default | notes |
|---|---|---|
| `general:gaps_in` / `gaps_out` | 5 / 20 | px between tiles / to screen edge |
| `general:border_size` | 1 | overlay thickness (DWM border is always 1px) |
| `general:col.active_border` / `col.inactive_border` | | `rgba(RRGGBBAA)`, `rgb()`, `0xAARRGGBB`, gradient + `Ndeg` |
| `general:border_overlay` | auto | auto = DWM on Win11, Direct2D overlay on Win10; `on` forces the gradient overlay |
| `general:layout` | dwindle | `dwindle` \| `master` |
| `decoration:rounding` | 0 | 0 square, 1-4 small, 5+ round (DWM corner preference, Win11) |
| `decoration:active_opacity` / `inactive_opacity` | 1.0 | layered-window alpha (template: inactive 0.88 = translucent unfocused windows) |
| `decoration:blur:enabled` / `backdrop` | false / acrylic | Win11 22H2+ system backdrop (`mica` \| `acrylic` \| `tabbed`) |
| `animations:enabled`, `bezier`, `animation` | | `windows`, `windowsIn` (`popin N%`), `windowsMove`; speed in 100ms units |
| `dwindle:default_split_ratio` / `spiral` | 1.0 / false | |
| `master:mfact` / `new_status` / `orientation` | 0.55 / slave / left | |
| `input:hotkey_fallback` | true | see Keybinds |
| `misc:float_dialogs` / `float_unresizable` / `notify_errors` | true | |
| `env = NAME, VALUE` | | environment for launched programs |
| `script = path.lua` | | Lua macros & rules |
| `bar:*` | disabled | see Top bar |
| `wallpaper:*` | grow, 1s | `transition duration bezier query sorting top_range categories purity atleast apikey directory` |

## Keybinds

**Win+F1** (or **Win+/**) opens a cheat sheet of every bind in your profile: grouped, searchable (just type), Esc to close, and an *Edit binds* button. The `binds` dispatcher toggles it.

`bind = MODS, KEY, DISPATCHER, ARGS` — modifiers `SUPER ALT CTRL SHIFT`.

Each bind is registered with `RegisterHotKey`. Explorer already owns most
`Win+<key>` combos (Win+E, Win+R, Win+1-9, Win+arrows…), which makes
`RegisterHotKey` fail. With `input:hotkey_fallback = true` those binds are
served by a `WH_KEYBOARD_LL` hook on its own thread instead, which swallows
the key so Explorer's action and the Start menu don't fire. `dbctl binds`
shows which mechanism each bind uses. Win+L cannot be overridden.

## Top bar (taskbar replacement)

`bar { enabled = true }` draws a bar on the top edge of every monitor and
`hide_taskbar = true` hides the Windows taskbar.

- Each bar is a real AppBar (`SHAppBarMessage`), so Windows shrinks the work
  area and tiled windows sit below it with the usual gaps.
- The bar runs on its own thread and redraws every second.
- Modules: `launcher workspaces apps title clock cpu memory temperature volume
  wifi bluetooth battery wallpaper power`, arranged with `modules-left/center/right`.
- Apps: click to focus (or minimise if already focused), middle-click to close.
  Workspaces: click to switch, scroll to step. Volume: scroll to change.
- Clicking a stat opens a flyout: calendar, CPU/RAM history, thermal zones,
  a volume slider with mute, Wi-Fi signal/adapter, Bluetooth devices,
  battery time, and power actions (lock / sleep / restart / shut down / sign out).

| stat | source |
|---|---|
| CPU | `GetSystemTimes` |
| memory | `GlobalMemoryStatusEx` |
| temperature | PDH `\Thermal Zone Information(*)\Temperature` (ACPI zones; hidden if none) |
| battery | `GetSystemPowerStatus` |
| Wi-Fi | WLAN API current connection (SSID needs location access on 24H2+) |
| Bluetooth | `BluetoothFindFirstRadio` / `BluetoothFindFirstDevice` |
| volume | Core Audio `IAudioEndpointVolume` |

The taskbar's original auto-hide state is saved to `%APPDATA%\deadbeef\taskbar.restore`
before anything changes. It is restored on exit, or on the next start if deadbeef
crashed. Explorer re-showing the taskbar (Start menu, hover, restart) is undone
from the `EVENT_OBJECT_SHOW` hook. Notification-area (tray) icons are not
re-hosted in the bar.

## App launcher (Win+R)

A built-in launcher in the style of PowerToys Run, drawn with the colours, border, font and prompt in `launcher.ini`. Type to fuzzy-search every installed app (desktop and Store, the same list as Start → All apps); apps you launch often rank first.

| type | does |
|---|---|
| `code` | fuzzy-search apps (Enter opens, Ctrl+Enter as administrator) |
| `12*8+3` or `= sqrt(2)` | calculator; Enter copies the result |
| `> command` | run a command |
| `? query` | web search |
| `C:\path` | open a file or folder |

Set `[main] command` in launcher.ini to a program (e.g. Flow Launcher) to use that instead; `builtin` uses deadbeef's own.

## Wallpapers

`SUPER + W` opens the picker: a grid of wallhaven.cc results (WinHTTP + WIC, no
extra dependencies). Enter / Apply downloads the image (with progress) to
`wallpaper:directory`, closes the picker and applies it with an animated
transition. D / Download only downloads it. Tab shows already-downloaded
wallpapers, `/` searches, and PgUp/PgDn page through results.

The transition (`grow` from the cursor, `wipe`, `fade`, eased with any
configured bezier) is drawn in a layered window inserted directly above the
desktop, so it plays *behind* your windows. The wallpaper is then committed with
`IDesktopWallpaper` (Fill), and the overlay fades out once Explorer has
finished processing the image.

```
dbctl dispatch wallpaper                 # picker
dbctl dispatch wallpaper search nord     # picker with a query
dbctl dispatch wallpaper random          # random downloaded wallpaper
dbctl dispatch wallpaper set C:\pic.jpg  # apply a file with the transition
```

## Dispatchers

`exec` `launcher` `killactive` `togglefloating` `fullscreen [0|1]` `pin`
`centerwindow` `movefocus l|r|u|d` `movewindow l|r|u|d|mon:N` `swapwithmaster`
`cyclenext [prev]` `togglesplit` `splitratio ±D|exact N` `layout dwindle|master|toggle`
`layoutmsg …` `resizeactive DX DY` `moveactive DX DY` `focusmonitor N|±1`
`workspace N|±1|e±1` `movetoworkspace N[,WINDOW]` `movetoworkspacesilent N[,WINDOW]`
`focuswindow RULE` `closewindow RULE` `lua FN` `wallpaper …` `reload` `exit`
(`dbctl dispatchers` describes each.) `WINDOW` is `hwnd:0x…` or an `exe:`/`class:`/`title:` matcher.

Dwindle works like Hyprland's: every new window **splits the focused window in half** (side by side if that tile is wider than tall, stacked otherwise). Closing a window gives its space back to its neighbour. `togglesplit` (Win+J) flips the focused window's split and `splitratio` (Win+Ctrl+←/→) resizes it. Each workspace and monitor keeps its own split tree. `dwindle:spiral = true` switches to the fixed fibonacci spiral instead.

**Focus follows the mouse** (`input:follow_mouse = 1`): resting the cursor on a window for about 0.1 s makes it the active window. Focus doesn't change while a mouse button is held or while the focused app has a menu open. `follow_mouse = 0` restores click-to-focus.

**Mouse:** drag a tile's border to resize it (the neighbours shrink and grow live, and the new split sticks). Drag a tile by its title bar onto another tile to swap them.

**Minimum sizes:** apps that can't shrink below a certain size (Spotify ~1000x750, Chrome's profile picker ~780x815) are detected at runtime. The layout gives them room, rearranging splits (side by side vs stacked) if that helps. Only when no arrangement fits on the screen does the newest window float on top; it returns to the tiles as soon as another window closes.

Tiled windows are never left maximised: a window that opens maximised is tiled, and the maximise button (or a title-bar double-click) toggles `fullscreen 1` - the window fills the area below the bar but keeps `gaps_out` and its border. `fullscreen 0` covers the whole monitor.

While the task switcher, Task View or Start is open (Alt+Tab, three-finger swipe), translucency is suspended so thumbnails and animations look normal.

Workspaces are Windows virtual desktops: workspace N is desktop N, created on
demand. Desktops span all monitors, so `workspace = N, monitor:M` means
"windows sent to workspace N are placed on monitor M".

## dbctl

```powershell
dbctl reload
dbctl dispatch exec wt
dbctl keyword general:gaps_in 8        # until next reload
dbctl clients -j                       # also: monitors, workspaces, binds, activewindow
dbctl getoption decoration:rounding
dbctl errors
```

Exit code 0 on success, 1 on a command error, 2 if deadbeef isn't reachable.

## Lua

`script = …\deadbeef.lua` loads a Lua 5.4 script with a `deadbeef` table:
`dispatch(name, args)`, `keyword(key, value)`, `option(key)`, `clients()`,
`log(msg)`, `on("windowopen" | "focus", fn)`, `launcher` (launcher.ini).
A `windowopen` handler may return a rule table
(`{ float = true, workspace = 3, size = {"60%", "70%"} }`).
Each call has a 250ms budget, so a runaway script can't freeze tiling.

## Limitations

- Windows of elevated processes can't be moved unless deadbeef itself runs elevated (UIPI).
- Per-window translucency uses `WS_EX_LAYERED` on other apps' windows. A few apps
  (some games and video players) don't like that; exempt them with `windowrule = opacity 1.0, exe:…`.
- DWM borders are a single colour (the first gradient stop); use `border_overlay = on` for gradients.
- `monitor` scale is informational — Windows owns per-monitor scaling. Explicit
  resolutions/positions are applied with `ChangeDisplaySettingsEx` and persist.
- Switching desktops and moving windows of other processes uses the shell's
  undocumented virtual-desktop interfaces via `winvd` (requires Windows 11 24H2+
  for this winvd version); "is this window on the current desktop" uses the
  documented `IVirtualDesktopManager`.

## Layout

```
src/main.rs            message loop, hidden window, wiring
src/state.rs           clients, manage/unmanage, focus, decorations, reload
src/dispatch.rs        dispatchers
src/wm/                hook.rs (SetWinEventHook) · tiler.rs (SetWindowPos) · workspace.rs · rules.rs
src/layout/            dwindle.rs · master.rs (pure, unit-tested)
src/config/            parser.rs (recursive descent) · types.rs · profile.rs · launcher.rs
src/animation/         bezier.rs (Newton-Raphson + 256-entry LUT) · ticker thread (DwmFlush-paced)
src/input/hotkey.rs    RegisterHotKey + WH_KEYBOARD_LL fallback
src/decoration/        dwm.rs (Win11) · overlay.rs (Direct2D layered window)
src/bar/               top bar thread · sysinfo.rs (stats) · clock.rs
src/shell/             taskbar hide/restore
src/ui/                Direct2D/DirectWrite layered-window canvas · image.rs (WIC)
src/wallpaper/         http.rs (WinHTTP) · wallhaven.rs · picker.rs · transition.rs · desktop.rs
src/settings/          settings app: mod.rs (window, widgets, pages) · model.rs (options ⇄ settings.conf)
                       preview.rs · system.rs (install, autostart, start/stop, reset, uninstall)
src/tray/  src/ipc/  src/script/ (Lua)
dbctl/src/main.rs      CLI client
templates/nerdy-deadbeef/
```
