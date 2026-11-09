# osu!tab guide

Everything about using osu!tab. For an overview, see the [README](../README.md). Numbers and tablet details here come from testing with a Samsung Galaxy Tab S8 Ultra, so they may differ on other hardware.

## Getting started

1. On the tablet, turn on **USB debugging**. Go to Settings > About tablet > Software information and tap Build number seven times. Then go to Settings > Developer options and turn on USB debugging.
2. Connect the tablet to your PC with a USB cable.
3. Run `osu!tab.exe`, then answer its two questions: how the tablet sits, and how big the pen area should be. If Windows says "Windows protected your PC", click More info, then Run anyway: osu!tab isn't code-signed yet.
4. On the tablet, tap **Allow** on the "Allow USB debugging?" prompt, and tick "Always allow from this computer". The Play tab says when it's waiting for this. The first connection installs the osu!tab app on the tablet, which takes a few seconds.
5. In lazer, turn off **High precision mouse** (Settings > Input > Mouse).

The **Play** tab checks all of this and tells you what's left. When it says "Ready to play", you're set.

## Using it

Closing the window keeps osu!tab running in the tray, so it's out of the way while you play. Click the tray icon to bring it back. **Quit**, in the window or the tray menu, stops osu!tab, gives the pen back to Android and closes the tablet app. Starting osu!tab again while it's running just brings up its window.

Unplugging and replugging recovers by itself, and so does changing your resolution or main monitor. A dropped connection usually comes back in about 0.3 s. Settings are saved as you change them.

### The Play tab's checks

The Play tab lists anything that could get in the way, with what to do about it. Passed checks fold into one line. It checks that:

- the tablet is connected, and whether its "Allow USB debugging?" prompt is waiting for you
- osu!lazer's High precision mouse is off, and whether its frame limiter or screen mode adds delay
- no apps known to cause stutters are running
- the tablet app is open and full screen
- Windows hasn't held up the cursor in the last 15 minutes. If it did, it says for how long and which program was in front.
- in OpenTabletDriver mode: OpenTabletDriver is included, receiving the pen, not running as administrator, not using Windows Ink, and has a pen area for the S Pen

### Before you play

- Close any apps you don't need, especially overlays, animated wallpapers, mouse and lighting software and desktop utilities. In testing, closing a few background apps took the worst delay during play from 91.6 ms to 16.8 ms. The Play tab warns you about some known ones.
- Leave the precision test off. It turns itself off after 2 minutes, and when you close the window.

### The pen's side button

On the Pen area tab, click the button next to **Side button key**, then press the key you want and let go. Any key Windows can see works, including Esc, `` ` ``, the function keys, Ctrl, Shift, the arrows and the numpad.

It can also be a combination of up to three keys, such as **Shift + 2** for `!` or **Shift + Alt + 2** for a hotkey: hold the keys down together, then let go. Nothing is chosen until you let go of a key, so Shift on its own still works too. Ctrl, Shift, Alt and Win always go first, whatever order you pressed them in, and the button presses the keys in that order and lets go of them the other way round.

Inside osu!tab the keys you press only set the button, but Windows still reacts to a few keys, so Caps Lock will still switch and the Windows key will still open Start. **Clear** removes it.

The Fn key on most laptops can't be chosen, because it's handled by the keyboard and Windows never sees it. Pressing Fn with another key sets whatever that pair sends, such as a function key or a volume key.

### The tablet's screen

The tablet app opens by itself and shows your pen area, its size in mm and the on-screen sensitivity. If the pen goes outside the area, the cursor stays pinned to the screen's edge.

To edit with your fingers, hold the circle in the corner until its ring fills, which takes a second unless you change it. Then drag inside the area to move it, and drag a corner dot or pinch to resize it. Tap the circle when you're done. Edit mode also ends after 20 seconds without changes, or the time you set, so a palm can't move the area during play.

Editing needs the PC, because that's where the area is saved. While osu!tab isn't connected, the tablet says so and ignores touches.

The tablet app keeps the screen on while osu!tab is connected. The PC checks in every 2 s, so if osu!tab ends or the PC sleeps, the app notices within 6 s and shows that it's waiting. Two minutes later the screen can turn off as usual.

While osu!tab runs, Android can't detect the pen, so its palm rejection is off. The app takes every touch itself, so a palm can't press anything in it. Samsung's floating S Pen icon can still be touched. To remove it, turn it off in Settings > Advanced features > S Pen > Air command.

The system bars stay hidden during normal use, but Android doesn't let an ordinary app block them completely. A swipe from the screen's edge brings them back, and the app hides them again about 150 ms later. The Home button can be pressed in that moment. To block Home and Back, turn on **Pin the app** on the Settings tab. It's off by default, because on this tablet leaving a pinned app sends you to the lock screen, so quitting osu!tab means entering your PIN. Android also resets the USB connection once when it pins the app, about a second after osu!tab starts, and osu!tab reconnects within a second. If the cable is pulled while the app is pinned, it stays pinned until you hold Back and Recents.

The app has to fill the screen, or the area it draws won't line up with the pen. osu!tab opens it full screen, even in Samsung's desktop mode, and starts it afresh each time so a lock screen left over from before can't sit on top. If Android still shows it in a floating window, the app keeps asking for full screen, and both the app and the Play tab tell you to use the full-screen button in the window's title bar.

### Themes

The **Appearance** tab has five built-in themes: Midnight, Daylight, OLED Black, Candy and High Contrast. A theme applies to the PC and the tablet straight away, and survives restarts.

You can change the colors, the area's outline color and thickness, and how dark the tablet is outside the area. A Reset link appears beside anything you've changed. **Save as my theme** keeps your changes under a name. There's also a switch to turn animations off on both apps. Animations can't affect pen latency, because they run on separate threads from the pen, and the tablet app isn't in the pen's path at all.

### Settings

Every setting below is on the **Settings** or **Appearance** tab, and in `config.toml`. A Reset link appears beside anything that isn't at its default.

| Setting | Default | Applies |
|---|---|---|
| `theme` | Midnight | Right away |
| `animations` | On | Right away |
| `color_background`, `color_panel`, `color_text`, `color_muted`, `color_accent` | From the theme | Right away |
| `outline_color`, `outline_thickness`, `outside_dim` | From the theme | Right away |
| `show_area_label` (size text on the tablet) | On | Right away |
| `hold_to_edit_seconds` | 1 | Right away |
| `edit_timeout_seconds` | 20 | Right away |
| `close_to_tray` | On | Right away |
| `start_minimized` | Off | Next start |
| `open_tablet_app` (opens it when the tablet connects, and closes it when osu!tab quits) | On | Next connect |
| `screen_pinning` | Off | Next connect |
| `usb_path` (`direct` or `adb`) | `direct` | Next connect |

### Direct USB

By default osu!tab talks to the tablet's debugging service itself, over USB, so Google's `adb.exe` isn't needed at all. It installs the tablet helper and app over the same connection, and makes its own key the first time if this PC never had adb, in `%USERPROFILE%\.android`, where adb keeps it too. In testing, direct USB was slightly faster than going through `adb.exe` and steadier when the CPU was busy. See [Reading the latency numbers](#reading-the-latency-numbers). The Settings tab shows whether the link is running at USB 2 or USB 3 speed.

It also starts much faster: the pen was ready about 0.15 s after launching, against about 5.4 s through `adb.exe`, which takes about 5 seconds just to start.

Anything else using adb on this PC, such as Android Studio, loses the tablet until osu!tab quits, because osu!tab stops a running adb server to reach the tablet. The other USB path, `adb`, goes through `adb.exe` and needs Google's [platform-tools](https://developer.android.com/tools/releases/platform-tools) folder beside `osu!tab.exe`. It's also used when the orientation is set to follow the screen, and as a fallback: if direct USB fails, osu!tab says so on the Play tab and uses adb.exe until the next plug-in.

### Choosing a pen area

The whole tablet is about 314 x 196 mm. Mapped to a 2560 x 1600 screen, that's 8.2 px per mm, so jumps take a long arm movement and it feels slow. A smaller area feels faster:

| Area width | Height at 16:10 | 1 mm of pen movement |
|---|---|---|
| 314 mm | 196 mm | 8.2 px |
| 150 mm | 94 mm | 17.1 px |
| 120 mm | 75 mm | 21.3 px |
| 100 mm | 62.5 mm | 25.6 px |
| 80 mm | 50 mm | 32 px |

**Lock to screen shape** keeps the area's shape the same as your screen's, so the same movement goes as far across as up and down. Turn it off if you prefer a stretched area. Give your hand some room by keeping the area away from the tablet's edges.

### Filters

The **Pen area** tab has two smoothing filters for direct output, so a shaky hand doesn't need OpenTabletDriver. Both start off, because each adds some delay. Changes apply straight away, and both start over each time the pen leaves range, so the cursor never slides in from where you lifted the pen.

- **Radial follow** holds the cursor still through small wobbles and follows bigger moves quickly. Its settings are the ones in OpenTabletDriver's Radial Follow Smoothing plugin, in tablet millimetres: the cursor trails the pen by at most the **outer radius** (1 mm by default), and ignores moves within the **inner radius** (0 mm). **Smoothing** (0.95) is how much of the remaining distance each report keeps, so higher is smoother but slower to settle. **Soft knee** (1) eases the change at the outer radius, and **leak** (0) lets some smoothing carry on past it. Smoothing works per report, like the plugin's, so values you used through OpenTabletDriver with this tablet feel the same here. It's written from the plugin's description, so its curve can differ slightly.
- **Time smoothing** averages the pen's position over a short time (5 ms by default). The cursor catches up about two-thirds of a move in that time, and slow moves trail the pen by about that much. It's timed by the tablet's own report times, so it feels the same however often the pen reports.

With both on, time smoothing runs first. In OpenTabletDriver mode these are hidden and OpenTabletDriver's own filters apply.

## osu! settings

### osu!lazer (tested)

Settings > Input:

| Setting | Value | Why |
|---|---|---|
| High precision mouse | **Off** | When on, lazer moves its cursor by relative movement instead of the pen's position, so jumps won't land under the pen. osu!tab warns you if it's on. |
| Cursor sensitivity | Doesn't matter | It only applies when High precision mouse is on. |
| Map absolute input to window | Off | osu!tab already maps to the whole screen. |
| Disable mouse buttons during gameplay | Your choice | When on, the pen tip won't hit notes, so hit with Z/X or the side button. |
| Touch input | Off | osu!tab never sends touch. Turning this off stops any other touch device from adding the Touch Device mod. |

Lazer's Tablet section says no tablet is detected, which is expected and happens in both output modes. Lazer has its own built-in copy of OpenTabletDriver, which only looks for real USB tablets from its built-in list and doesn't load plugins. osu!tab's own settings do that job: size, position, rotation, shape lock and presets. Making lazer detect a tablet would need a third-party kernel driver and an impersonated Wacom ID.

### osu!stable (not tested)

A starting point, in Options > Input: Raw input off, OS TabletPC support off, sensitivity 1.0.

### Windows

No special settings are needed. Direct mode never touches Windows Ink, so there's nothing to turn off, and "Enhance pointer precision" doesn't affect absolute positioning. In OpenTabletDriver mode, use Absolute Mode. osu!tab warns you if the output is set to Windows Ink.

## OpenTabletDriver plugins

Choose the output on the **Play** tab:

| Output | Path | Use it for |
|---|---|---|
| **Direct** (default) | Pen > osu!tab > `SendInput` | The lowest latency, with osu!tab's own [filters](#filters) if you want them. OpenTabletDriver's filters and plugins don't apply. |
| **OpenTabletDriver** | Pen > osu!tab > local socket > OpenTabletDriver > its filters > its output | OpenTabletDriver's filters and plugins. It sets the area, filters and buttons in this mode, not osu!tab. |

osu!tab includes its own copy of OpenTabletDriver and a bridge plugin that presents the S Pen to it as a tablet called "osu!tab S Pen". It needs the [.NET 10 Desktop Runtime](https://dotnet.microsoft.com/download/dotnet/10.0). It uses your normal `%LOCALAPPDATA%\OpenTabletDriver` folder, and only touches it once you choose this output: it adds the tablet's configuration, and backs up OpenTabletDriver's settings as `settings.before-osu!tab.json` before changing them. A profile from before the rename, under "osutab S Pen", carries over with its area. Install community plugins with OpenTabletDriver's Plugin Manager.

Set the pen area in OpenTabletDriver, in Absolute Mode. osu!tab draws that area on the tablet and on its Play and Pen area pages, within a second of a change, whichever way the tablet is held. OpenTabletDriver sees the tablet in landscape, 313.8 × 195.9 mm, so if you hold it in portrait, set the area's rotation there to 90 or 270. The tablet can't edit this area, since it belongs to OpenTabletDriver.

OpenTabletDriver doesn't work properly as administrator, so osu!tab starts its daemon and settings window without administrator rights, even when osu!tab itself has them. If one is already running as administrator, the Play tab says so: quit it from its tray icon and osu!tab starts it again.

OpenTabletDriver's daemon only loads plugins and tablet configurations as it starts. So when you pick OpenTabletDriver mode and osu!tab has to update its plugin or configuration, or remove a second configuration for the S Pen that an older osu!tab left, it restarts a daemon that's already running. Two configurations for the one pen would make two tablets, each with its own area, and the cursor would jump between them.

What has been tested:

- osu!tab starts the daemon in about 0.4 s, the bridge plugin connects about 1.5 s later, and the tablet draws OpenTabletDriver's area where it maps the pen.
- With simulated pen reports, OpenTabletDriver finds the tablet, and takes about 0.15 ms (median) from the report being sent to the cursor moving.
- A per-report filter and a timed filter at 1000 Hz, both written for this test, load and move the cursor by the expected amount. The timed filter's median is about 0.68 ms, because it waits for its next tick.

What hasn't been tested: the S Pen itself in this mode, and any community plugin. Plugins that only change position through OpenTabletDriver's filter interfaces should work. A plugin that talks to specific tablet hardware or depends on its button layout might not.

## Troubleshooting

| Problem | What to do |
|---|---|
| "Tablet not found" | Check the cable carries data, and that USB debugging is on. Windows normally sets up the tablet's USB connection by itself. If it's still not found, install Samsung's [Android USB Driver for Windows](https://developer.samsung.com/android-usb-driver). |
| "Waiting for your OK on the tablet" | Tap Allow on the tablet. With no prompt, replug the cable with the tablet unlocked. Still nothing? Revoke USB debugging authorizations in Developer options, then replug. |
| "adb.exe not found" | Only the `adb` USB path needs it. Switch to Direct USB on the Settings tab, or put Google's [platform-tools](https://developer.android.com/tools/releases/platform-tools) folder next to `osu!tab.exe`. |
| "Windows protected your PC" | Click More info, then Run anyway. osu!tab isn't code-signed yet. |
| Jumps don't land under the pen | Turn off High precision mouse in lazer. |
| It feels slow | Make the pen area smaller. |
| Occasional stutters | Close any apps you don't need. See "Before you play". The Play tab says when Windows held up the cursor, and which program was in front. |
| The pen doesn't work on the tablet itself | osu!tab holds the pen while it runs. Quit osu!tab to give it back. |
| OpenTabletDriver mode doesn't start | Install the [.NET 10 Desktop Runtime](https://dotnet.microsoft.com/download/dotnet/10.0). |
| "OpenTabletDriver has no pen area for the S Pen yet" | Open OpenTabletDriver, pick osu!tab S Pen, and set an area in Absolute Mode. |
| In OpenTabletDriver mode the cursor jumps back and forth | OpenTabletDriver has the S Pen twice, usually because an older osu!tab added a second "osutab S Pen". This version removes it and restarts OpenTabletDriver when you pick its mode. Don't run an older copy of osu!tab, which adds it back. |
| The connection drops for a few seconds now and then | Use Direct USB on the Settings tab: it reconnects in about 0.3 s, against about 5 s through adb.exe. A drop that's back within 3 s isn't shown. |

## How it works

A small helper runs on the tablet through Android's USB debugging connection and reads the S Pen directly from `/dev/input`, without root. Each pen report goes over USB to the Windows program, which moves the cursor with `SendInput`. Finger and palm input is on a separate device that the helper never opens. The mapping is linear and absolute, with no smoothing or acceleration.

Linux normally filters the pen's position a little: on the test tablet it ignored movements under 0.02 mm and averaged those up to 0.08 mm. On a small pen area that's a few screen pixels, so osu!tab turns the filter off while it holds the pen and puts it back when it lets go. Since the cursor can only sit on whole pixels, osu!tab aims at the middle of the right pixel, so rounding in Windows can't land it on the next one.

The tablet app talks to the PC over a separate link and doesn't handle pen input, so it isn't in the pen's path.

### Following the tablet's rotation

With the orientation set to follow the screen, the tablet app tells the PC whenever the screen turns, and the pen's mapping changes with it. Without the app running, the rotation is only read when the tablet connects.

### Reading the latency numbers

"Added latency" runs from the kernel's pen event timestamp to `SendInput` returning. It's the sum of these stages:

| Stage | Measured on |
|---|---|
| Pen event to helper sending | The tablet's clock |
| USB and adb to the PC | Both clocks, aligned using round trips, so it's only accurate to a fraction of a millisecond |
| PC to SendInput finished | The PC's clock |
| SendInput to cursor moved | The PC's clock, only while the precision test is on |

It doesn't include the S Pen's own hardware delay, the game or your display.

Measured with simulated reports at 480 Hz over 20 s, three runs of each, without `SendInput`, in ms. "Busy" has every CPU core kept busy at normal priority, as a harsh stand-in for a game.

| | PC | Median | 99th percentile | 99.9th percentile | Worst |
|---|---|---|---|---|---|
| adb.exe, without the tuning below | Quiet | 0.67–0.74 | 1.00–1.09 | 1.5–6.5 | 12.3–12.7 |
| adb.exe | Quiet | 0.54–0.56 | 0.84–0.92 | 1.5–7.8 | 12.3–14.6 |
| Direct USB | Quiet | 0.53–0.54 | 0.79–0.83 | 1.3–3.8 | 12.2–12.4 |
| adb.exe, without the tuning below | Busy | 0.97–1.49 | 20.9–37.5 | 29–53 | 33–63 |
| adb.exe | Busy | 0.49–0.55 | 0.77–2.19 | 1.8–10.8 | 13.0–17.9 |
| Direct USB | Busy | 0.38–0.54 | 0.66–0.81 | 1.4–2.1 | 4.4–12.7 |

The tuning is what osu!tab does by default:

- It raises `adb.exe` to high priority, which made most of the difference on a busy PC.
- It runs the tablet helper on the tablet's faster cores, which took about 0.13 ms off the median.
- It stops Windows treating osu!tab and `adb.exe` as background work to slow down.

Four things that were tried and left out, because they didn't reliably help: Windows' multimedia scheduler, keeping threads on one group of the CPU's cores, WinUSB's `RAW_IO` setting, and having the PC spin on the USB read instead of sleeping. In three rounds of each, the last two gave medians of 0.52–0.55 and 0.52–0.56 ms, against 0.52–0.56 ms without. That suggests little of the remaining time is on the PC, though only the PC's own stages can be measured directly. A worst case of about 12 ms shows up on every path, and its cause hasn't been found.

Real pen sessions also show occasional stalls of about 20 ms as the pen starts moving, and they don't appear with simulated reports. Their cause hasn't been found either.

## Files

Settings are in `%APPDATA%\osu!tab\config.toml`. Versions before the rename kept them in `%APPDATA%\osutab`, and the first run of this version moves that folder. The file is plain text, so you can edit it by hand while osu!tab is closed, in any editor, including ones that save with a byte order mark. Older files keep working, and anything missing gets its default. If a line has a mistake, only that setting goes back to its default. osu!tab tells you which, and keeps a copy of the old file next to it as `config-problem-<number>.toml`. A latency log for each run sits next to it, and only the newest 30 are kept. The Latency tab opens that folder.

## Building from source

You'll need [Rust](https://rustup.rs) and the .NET 10 SDK. The tablet app needs Java 21, Gradle 8.11 and the Android SDK (platform 35, build-tools 35). `build.ps1` looks for them in `tools\jdk21`, `tools\gradle` and `tools\android-sdk`, and fetches OpenTabletDriver's source at commit `f3927ac` into `tools\otd-src`. Google's platform-tools in `tools\platform-tools` are optional, for the `adb` USB path.

```powershell
rustup target add aarch64-unknown-linux-musl
.\build.ps1
.\target\release\osutab.exe
```

Cargo can't put a "!" in a program's name, so the build is `osutab.exe`. `package.ps1` builds everything and puts a copy you can move anywhere in `dist\osu!tab`, with the program renamed `osu!tab.exe`. Nothing from Google is included. `package.ps1 -Zip` also makes the release zip and its SHA-256.

To run the tests:

```powershell
cargo test
```

## Command-line options

Started from a terminal, osu!tab prints its messages there.

| Option | Meaning |
|---|---|
| `--setup` | Shows the setup questions again. |
| `--rotation 0\|90\|180\|270` | Overrides the saved orientation for this run. |
| `--no-tip-click` | The pen tip doesn't click. |
| `--button-key KEY` | Maps the side button to a key for this run, such as `z`, `esc`, `f5` or `` ` ``, or up to three joined by `+`, such as `"left shift + 2"`. |
| `--no-grab` | Android keeps receiving the pen too. |
| `--no-gui` | Runs in the console with no window, and asks setup questions there. |
| `--no-app` | Doesn't install or open the tablet app. |
| `--tab NAME` | Opens on a tab: `play`, `area`, `latency`, `precision`, `appearance` or `settings`. |
| `--usb-path adb\|direct` | Overrides the saved USB path for this run. |
| `--no-tuning` | Leaves out the scheduling choices above, to measure what they do. |
| `--keep-pen-filter` | Leaves Linux's filter on the pen's position, to measure what turning it off does. |
| `--cpu-burn SECONDS` | Keeps every CPU core busy, to test with a loaded PC. No tablet needed. |
| `--duration SECONDS` | Stops automatically and prints a summary. |
| `--test-drops SECONDS` | Stops the tablet helper every so many seconds, to test how fast osu!tab recovers. |
| `--dry-run` | Measures without injecting any input, and sends nothing to OpenTabletDriver. |
| `--test-mode` | Starts with the precision test on, with no time limit. |
| `--self-test` | Moves the cursor to known positions and checks where it lands. No tablet needed. |
| `--stall-trace DIR` | Records what happened around each latency stall of 20 ms or more. Adds watchdogs on both sides and pings every 10 ms. |
| `--sendinput-bench SECONDS` | Times `SendInput` alone with zero-distance moves. Can run alongside a game. No tablet needed. |
| `--burst ON_MS OFF_MS` | Sends fake pen reports in bursts, to test pauses without the pen. Use with `--dry-run`. |
| `--ping-bench` | Measures USB round trips about 480 times a second. No pen needed. |
| `--usb-bench SECONDS` | Measures the same round trips directly over USB, without adb.exe. Stop the adb server first. |
| `--otd-bench SECONDS` | Sends simulated reports through OpenTabletDriver and times the cursor. This moves your cursor. |
| `--ping-ms MS` | Sets how often to ping the tablet. |
| `--busy-poll` | The PC program spins instead of waiting for data. In benchmarks this didn't help. |
| `--helper-busy` | The tablet helper spins instead of waiting, using one tablet CPU core constantly. |
