# osutab

Turns a Galaxy Tab S8 Ultra and S Pen into a USB pen tablet for osu! on Windows, with about 0.8 ms of added latency. It's free, needs no root and no kernel driver, and connects only by the USB cable.

Tested in osu!lazer. It hasn't been tried in osu!stable yet.

## Getting started

1. On the tablet, turn on **USB debugging**. Go to Settings > About tablet > Software information and tap Build number seven times. Then go to Settings > Developer options and turn on USB debugging.
2. Connect the tablet to your PC with a USB cable.
3. Run `osutab.exe`, then answer its two questions: how the tablet sits, and how big the pen area should be.
4. On the tablet, tap **Allow** on the "Allow USB debugging?" prompt, and tick "Always allow from this computer". Approve the osutab app if Android asks.
5. In lazer, turn off **High precision mouse** (Settings > Input > Mouse).

The **Play** tab checks all of this and tells you what's left. When it says "Ready to play", you're set.

## Using it

Closing the window keeps osutab running in the tray, so it's out of the way while you play. Click the tray icon to bring it back. **Quit**, in the window or the tray menu, stops osutab and gives the pen back to Android. Starting osutab again while it's running just brings up its window.

Unplugging and replugging recovers by itself. Settings are saved as you change them, and changes apply straight away.

### Before you play

- Close **PowerToys**, **Wallpaper Engine** and the **NVIDIA overlay**. In testing, closing all three took the worst delay during play from 91.6 ms to 16.8 ms. The Play tab warns you if any are running. Which of them causes it hasn't been isolated.
- Leave the precision test off. It turns itself off after 2 minutes, and when you close the window.

### The tablet's screen

The tablet app opens by itself and shows your pen area, its size in mm and the on-screen sensitivity. If the pen goes outside the area, the cursor stays pinned to the screen's edge.

To edit with your fingers, hold the circle in the corner for a second. Then drag inside the area to move it, and drag a corner dot or pinch to resize it. Tap the circle when you're done. Edit mode also ends after 20 seconds without changes, so a palm can't move the area during play.

While osutab runs, Android can't detect the pen, so its palm rejection is off. The app takes every touch itself, so a palm can't press anything in it. A palm can still reach edge swipes that show the system bars, and Samsung's floating S Pen icon. To remove the icon, turn it off in Settings > Advanced features > S Pen > Air command.

### Choosing a pen area

The whole tablet is about 314 x 196 mm. Mapped to a 2560 x 1600 screen, that's 8.2 px per mm, so jumps take a long arm movement and it feels slow. A smaller area feels faster:

| Area width | Height at 16:10 | 1 mm of pen movement |
|---|---|---|
| 314 mm | 196 mm | 8.2 px |
| 150 mm | 94 mm | 17.1 px |
| 120 mm | 75 mm | 21.3 px |
| 100 mm | 62.5 mm | 25.6 px |
| 80 mm | 50 mm | 32 px |

Keep the area's shape the same as your screen's. A stretched area makes the same movement go further one way than the other, and osutab warns you about it. Give your hand some room by keeping the area away from the tablet's edges.

## osu! settings

### osu!lazer (tested)

Settings > Input:

| Setting | Value | Why |
|---|---|---|
| High precision mouse | **Off** | When on, lazer moves its cursor by relative movement instead of the pen's position, so jumps won't land under the pen. osutab warns you if it's on. |
| Cursor sensitivity | Doesn't matter | It only applies when High precision mouse is on. |
| Map absolute input to window | Off | osutab already maps to the whole screen. |
| Disable mouse buttons during gameplay | Your choice | When on, the pen tip won't hit notes, so hit with Z/X or the side button. |
| Touch input | Off | osutab never sends touch. Turning this off stops any other touch device from adding the Touch Device mod. |

Lazer's Tablet section says no tablet is detected, which is expected. osutab's own settings do that job: size, position, rotation, shape lock and presets. Making lazer detect a tablet would need a third-party kernel driver and an impersonated Wacom ID.

### osu!stable (not tested)

A starting point, in Options > Input: Raw input off, OS TabletPC support off, sensitivity 1.0.

### Windows

No special settings are needed. Windows Ink options and "Enhance pointer precision" don't affect absolute positioning.

## Troubleshooting

| Problem | What to do |
|---|---|
| "Tablet not found" | Check the cable carries data, and that USB debugging is on. |
| "Waiting for your OK on the tablet" | Tap Allow on the tablet. With no prompt, replug the cable with the tablet unlocked. Still nothing? Revoke USB debugging authorizations in Developer options, then replug. |
| "adb not found" | Put Google's [platform-tools](https://developer.android.com/tools/releases/platform-tools) folder next to `osutab.exe`. |
| Jumps don't land under the pen | Turn off High precision mouse in lazer, and fix any stretched area. |
| It feels slow | Make the pen area smaller. |
| Occasional stutters | Close the apps listed under "Before you play". Delays of 20–85 ms can still happen in menus and just after leaving a map. None were seen during play with those apps closed. |
| The pen doesn't work on the tablet itself | osutab holds the pen while it runs. Quit osutab to give it back. |

## How it works

A small helper runs on the tablet through adb and reads the S Pen directly from `/dev/input`, without root. Each pen report goes over USB to the Windows program, which moves the cursor with `SendInput`. Finger and palm input is on a separate device that the helper never opens. The mapping is linear and absolute, with no smoothing or acceleration.

The tablet app talks to the PC over a separate link and doesn't handle pen input, so it isn't in the pen's path.

### Reading the latency numbers

"Added latency" runs from the kernel's pen event timestamp to `SendInput` returning. It's the sum of these stages:

| Stage | Measured on |
|---|---|
| Pen event to helper sending | The tablet's clock |
| USB and adb to the PC | Both clocks, aligned using round trips, so it's only accurate to a fraction of a millisecond |
| PC to SendInput finished | The PC's clock |
| SendInput to cursor moved | The PC's clock, only while the precision test is on |

It doesn't include the S Pen's own hardware delay, the game or your display.

## Files

Settings are in `%APPDATA%\osutab\config.toml`. It's plain text, so you can edit it by hand while osutab is closed. A latency log for each run sits next to it, and only the newest 30 are kept. The Latency tab opens that folder.

## Building from source

The tablet app needs Java 21, Gradle 8.11 and the Android SDK (platform 35, build-tools 35). `build.ps1` looks for them in `tools\jdk21`, `tools\gradle` and `tools\android-sdk`. Put Google's platform-tools in `tools\platform-tools`.

```powershell
rustup target add aarch64-unknown-linux-musl
.\build.ps1
.\target\release\osutab.exe
```

`package.ps1` builds everything and puts a copy you can move anywhere in `dist\osutab`. That folder includes Google's adb, whose terms don't allow redistributing it, so keep it for your own use rather than publishing it.

To run the tests:

```powershell
cargo test
```

## Command-line options

Started from a terminal, osutab prints its messages there.

| Option | Meaning |
|---|---|
| `--setup` | Shows the setup questions again. |
| `--rotation 0\|90\|180\|270` | Overrides the saved orientation for this run. |
| `--no-tip-click` | The pen tip doesn't click. |
| `--button-key KEY` | Maps the side button to a letter or digit for this run. |
| `--no-grab` | Android keeps receiving the pen too. |
| `--no-gui` | Runs in the console with no window, and asks setup questions there. |
| `--no-app` | Doesn't install or open the tablet app. |
| `--tab NAME` | Opens on a tab: `play`, `area`, `latency` or `precision`. |
| `--duration SECONDS` | Stops automatically and prints a summary. |
| `--dry-run` | Measures without injecting any input. |
| `--test-mode` | Starts with the precision test on, with no time limit. |
| `--self-test` | Moves the cursor to known positions and checks where it lands. No tablet needed. |
| `--stall-trace DIR` | Records what happened around each latency stall of 20 ms or more. Adds watchdogs on both sides and pings every 10 ms. |
| `--sendinput-bench SECONDS` | Times `SendInput` alone with zero-distance moves. Can run alongside a game. No tablet needed. |
| `--burst ON_MS OFF_MS` | Sends fake pen reports in bursts, to test pauses without the pen. Use with `--dry-run`. |
| `--ping-bench` | Measures USB round trips about 480 times a second. No pen needed. |
| `--ping-ms MS` | Sets how often to ping the tablet. |
| `--busy-poll` | The PC program spins instead of waiting for data. In benchmarks this didn't help. |
| `--helper-busy` | The tablet helper spins instead of waiting, using one tablet CPU core constantly. |
