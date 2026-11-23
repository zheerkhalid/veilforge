# Changelog

What changed in each version of VeilForge, in plain words.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [1.1.0] - 2026-11-24

Everything you protected with 1.0.0 stays protected and opens as before. Nothing has to be unlocked before updating.

### Added

- **Kurdish, Arabic, Persian and Hebrew names are shown properly.** Letters are joined and the text runs in the right direction, the same way File Explorer shows it. This works in the list, the details panel, the dialogs, the messages and for group names.
- **A hotkey that locks everything.** Choose one in Settings (for example Ctrl+Alt+L). From anywhere, it locks every item you had opened for now and locks VeilForge itself.
- **`VeilForge.exe --lock-now`** does the same from a shortcut, a script or the Task Scheduler.
- **Ctrl+L** locks VeilForge while its window is in front.
- **Wrong-password log.** Every wrong master password is noted: when, and where it was typed (the VeilForge window, a protected item's password prompt, or "Restore everything"). What was typed is never stored. After you unlock, VeilForge tells you if someone tried while you were away. The list is in Settings, with a button to clear it.
- **The lock screen shows items that are still open.** If you opened items for now and VeilForge then locked itself, the lock screen says so and has a "Lock them now" button. Locking never needs the password.
- **Lock VeilForge when its window is closed** (a new setting, on by default).
- **The tray icon says what is going on** when the mouse rests on it: locked, unlocked, or how many items are open.
- **The details panel says when an open item locks again** on its own, following your settings.
- **Drop more files onto the open "Protect" dialog** to add them to its list. The drop hint also says how many items you are holding.

### Changed

- Closing the window to the tray now also locks VeilForge, so opening it again asks for the master password. You can switch this off in Settings, under "Locking again".
- "Lock now" in the tray menu now also reaches items that were opened through their stand-in shortcut while the main window was locked.
- The waiting time after wrong passwords is now shared by the window and the password prompts, and it carries on after VeilForge is closed and started again.
- The portable ZIP and the installer now include this changelog.

### Fixed

- A hotkey that Windows refused, because another program already uses it, was shown as "Active". It now says "Used by another program".
- Closing VeilForge and starting it again reset the waiting time after wrong passwords.
- Kurdish and Arabic file names appeared as separate letters in the wrong order.
- For developers: the packaging script now follows `CARGO_TARGET_DIR` instead of assuming the build is in `target`.

## [1.0.0] - 2026-11-17

First release.

### Added

- Three ways to protect a file or folder behind one master password: **Lock** (visible, but cannot be opened), **Hide** (moved into a protected holding folder under a random name) and **Ask on open** (a stand-in that asks for the password and locks the item again when you are done).
- One list of everything protected, with groups, search, temporary and permanent unlock, lock all and unlock all.
- A journal that finishes or undoes an interrupted operation after a crash or power cut, an encrypted list with rolling backups, and a recovery copy kept with every item.
- Health check with repair, and **Panic restore** that puts everything back, also as `VeilForge.exe --restore-all`.
- Automatic locking after idle time and when Windows locks, sleeps or signs out.
- Tray icon, a hotkey that opens the window, optional right-click entries in File Explorer, an emergency sheet to print.
- Installer (which restores everything before uninstalling) and a portable ZIP.

[1.1.0]: https://github.com/zheerkhalid/veilforge/releases/tag/v1.1.0
[1.0.0]: https://github.com/zheerkhalid/veilforge/releases/tag/v1.0.0
