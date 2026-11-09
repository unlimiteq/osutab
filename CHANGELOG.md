# Changelog

Every version of osu!tab, newest first. Version numbers follow [Semantic Versioning](https://semver.org).

## [Unreleased]

### Added

- **The side button can press up to three keys together**, such as Shift + 2 for `!`, or Shift + Alt + 2 for a hotkey. Hold the keys down, then let go. Ctrl, Shift, Alt and Win go first, and the keys are let go in reverse. `--button-key` takes the same, such as `"left shift + 2"`.

### Changed

- The side button picker chooses when you let go of a key, not when you press it, so Shift can start a combination or be the key on its own. It shows the keys held so far.
- Choosing OpenTabletDriver mode restarts its daemon when osu!tab's plugin or tablet configuration changed, because a running daemon keeps the ones it started with.

### Fixed

- **In OpenTabletDriver mode the cursor jumped back and forth.** An older osu!tab's "osutab S Pen" configuration made OpenTabletDriver see the S Pen as two tablets, each with its own area, which took turns with the pen. osu!tab now removes any second configuration for the S Pen, and a second tablet can't take the pen while the first is connected.

## [1.0.0-beta.1] - 2026-11-08

The first public beta, and the first version called osu!tab. The download has everything it needs, with nothing to fetch from Google.

### Added

- **No adb.exe needed.** osu!tab sets up the tablet over its own USB connection: it copies the helper with adb's file transfer protocol and installs the app with Android's package manager, which took 2.5 s on a fresh tablet. A PC that never had adb.exe gets its own key, in the same files adb.exe uses.
- The Play tab says when the tablet's "Allow USB debugging?" prompt is waiting for you.
- In OpenTabletDriver mode, OpenTabletDriver's own pen area is drawn on the tablet and on the Play and Pen area pages, whichever way the tablet is held. A test checks that the outline lands where OpenTabletDriver really maps the pen.
- New checks on the Play tab: the tablet app is floating instead of full screen, the tablet app isn't open, Windows held up the cursor in the last 15 minutes (with the program that was in front), OpenTabletDriver is running as administrator, and OpenTabletDriver has no area for the S Pen yet.
- The version is shown on the Settings tab and in the tray icon's tooltip.
- The log says why each session ended, why a start was slow, and each time Windows held up the cursor for 200 ms or more.
- The PC checks in with the tablet app every 2 s, so the app notices within 6 s that the PC has gone, even with the cable still plugged in.
- `--test-drops SECONDS` stops the tablet helper on a timer, to test recovery.
- `package.ps1 -Zip` makes the release download and its SHA-256.

### Changed

- **Renamed to osu!tab.** The window, the tray, the tablet app, OpenTabletDriver's tablet and plugin, and the program (`osu!tab.exe`) all use the new name. Settings and logs move from `%APPDATA%\osutab` to `%APPDATA%\osu!tab` the first time this version runs, and an OpenTabletDriver profile under the old tablet name carries over with its area.
- Direct USB is now the default USB path.
- OpenTabletDriver's daemon and settings window start without administrator rights, even when osu!tab has them, because OpenTabletDriver doesn't work properly as administrator.
- The stretched-area check is gone. Any area shape is fine.
- The tablet app opens full screen from the start, and starts afresh each time osu!tab opens it.
- Quitting osu!tab closes the tablet app, which also ends pinning.
- The tablet app keeps the screen on only while the PC is connected. Two minutes after the PC goes, the screen can turn off as usual.
- The tablet helper waits 6 s without hearing from the PC before giving the pen back, up from 2 s, because Windows sometimes holds up a busy program for over a second.

### Fixed

- **Drops took 5 s or more to recover.** A failed USB transfer left WinUSB's pipe halted, so every quick reconnect failed and osu!tab fell back to adb.exe, which took 5.5 s and sometimes up to a minute. Opening the connection now resets both pipes and turns on automatic stall clearing. In testing, six forced drops each came back in 0.26 to 0.32 s.
- The tablet wasn't found on a PC without Samsung's USB driver.
- Quitting while pinned started adb.exe and left it running, so the next start found the tablet taken and went the slow way.
- A settings file saved with a UTF-8 byte order mark, as PowerShell and some editors do, lost its first line.
- After unpinning, the tablet app could be left behind the lock screen with the status and navigation bars over it, and stay that way at the next start.
- After osu!tab quit or crashed, or the PC went to sleep, the tablet app kept the tablet's screen on indefinitely.
- On Samsung's desktop mode, the tablet app could open in a floating window, where the area it draws doesn't line up with the pen.

### Known issues

- With **Pin the app** on, Android resets the tablet's USB connection about a second after osu!tab starts. osu!tab reconnects by itself in about 0.7 s.
- Tested only with a Galaxy Tab S8 Ultra and the S Pen that comes with it, playing osu!lazer on Windows 11.

## [0.3.0] - 2026-11-01

### Changed

- **Connects in about 0.15 s** on direct USB, down from about 5.4 s. Instead of starting adb.exe, osu!tab checks what the tablet has installed and starts everything over its own USB connection. Pinning and unpinning go the same way, and the window opens about 0.2 s sooner.
- Linux's fuzz filter on the pen is turned off while osu!tab holds the pen, and put back afterwards. It ignored movements under 0.02 mm and averaged those up to 0.08 mm, which is a few screen pixels on a small pen area.
- The cursor is aimed at the middle of the target pixel, so Windows' rounding can't land it on the next one.

## [0.2.0] - 2026-11-01

### Added

- The pen's side button can be any key on the keyboard, including Esc, `` ` ``, the function keys, the arrows and the numpad. Click the button, then press the key. `--button-key` takes the same names.

### Fixed

- The side button key could never be set, because the keyboard hook it relied on is never called in a program with a settings window.

## [0.1.0] - 2026-11-01

The first complete version, then called osutab, built from 2026-10-30.

- A small helper on the tablet reads the S Pen from the kernel, without root, and streams each report over USB. The Windows program places the cursor with `SendInput`: absolute and one-to-one, with no smoothing.
- Under 1 ms of added latency in testing, and steady with every CPU core busy.
- A settings window with Play, Pen area, Latency, Precision test, Appearance and Settings tabs, a tray icon, and checks before you play, such as osu!lazer's High precision mouse setting.
- A tablet app that shows the pen area full screen, lets you move and resize it with your fingers, and takes every touch so a resting palm can't open anything.
- Five themes for both apps, saved themes, presets, and a plain-text settings file where a bad line only costs that one setting.
- An OpenTabletDriver output mode with a bridge plugin, for OpenTabletDriver's filters and plugins.
- An opt-in direct USB path, optional pinning, and following the tablet's rotation as it turns.

The program reported 0.1.0 until 1.0.0-beta.1. The 0.2.0 and 0.3.0 tags were added afterwards, to mark those milestones in the history.

[Unreleased]: https://github.com/unlimiteq/osutab/compare/v1.0.0-beta.1...HEAD
[1.0.0-beta.1]: https://github.com/unlimiteq/osutab/compare/v0.3.0...v1.0.0-beta.1
[0.3.0]: https://github.com/unlimiteq/osutab/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/unlimiteq/osutab/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/unlimiteq/osutab/releases/tag/v0.1.0
