# Latchmix

A tiny Windows 11 tray volume mixer that **remembers your volume and mute settings and keeps them**.

Set Spotify to 30%, mute Discord, mute your speakers: Latchmix puts them back whenever Windows, a game or the app itself changes them, after a restart, and when a device or app shows up again.

It is written in Rust against the Win32 and Core Audio APIs directly. No runtime, no framework, no polling.

## What it does

- **Left click** the tray icon: a flyout with the master volume and mute of the current output device, plus one slider and mute button per app that has an audio session. Mouse wheel over a row changes it in 2% steps. A thin level bar under each slider shows the live output peak (60 dB scale).
- **Middle click** the tray icon: mute or unmute the current output device.
- **Right click** the tray icon: pick the default output device (the active one is checked). It sets the console, multimedia and communications roles.
- **Remembers what you set.** Moving a slider or pressing mute pins that value for that device or app. A small pin shows next to pinned rows. Right click a row and choose *Forget saved volume and mute* to unpin it.
- **Keeps it.** Any outside change to a pinned value is reverted immediately, including the Windows volume keys and apps that reset their own volume.
- **Re-applies on start and on arrival.** Pinned values are applied at launch and whenever the device or app appears again.
- Follows the system light/dark theme and accent color, per-monitor DPI, and a taskbar on any edge.

Volume and mute are pinned independently: muting an app does not pin its volume, and the other way around. Apps are identified by executable name, devices by their Windows endpoint ID.

## Install

Build from source (below) and run `target\release\latchmix.exe`. There is nothing to install and no installer. To start it with Windows, right click the tray icon and tick *Start with Windows* (it writes one value under `HKCU\...\Run`; untick it to remove it).

## Build

Requirements: Windows 10 or 11, the Rust toolchain (stable, MSVC target) and the Visual Studio Build Tools with the C++ workload.

```
cargo build --release
cargo test
```

The result is a single ~300 KB executable. The file icon and version info are embedded with `rc.exe` from the Windows SDK; if the SDK is not installed the build still works, just without them.

## Where settings live

`%APPDATA%\latchmix\prefs.tsv`, one line per pinned item: `key<TAB>volume percent<TAB>mute`. A `-` means "not pinned". It is plain text and safe to edit while the app is closed. Nothing is sent anywhere.

## How it stays light

- Everything is event driven. Core Audio callbacks (endpoint volume, session volume, new sessions, device changes) only post a message to the main window, and bursts are coalesced into one. There are no timers and no polling while the flyout is closed. While it is open, one 33 ms timer reads the level meters and repaints only when a bar moved.
- The flyout is one window drawn with GDI into a memory bitmap. Shapes are anti-aliased by a small signed-distance rasteriser. The bitmap and fonts are freed when the flyout closes.
- A correction loop breaker pauses enforcement for 5 seconds if something keeps fighting a pinned value.

Measured on a Ryzen 5 3600 / Windows 11 (release build):

| | Latchmix | EarTrumpet 2.3 (same PC, same time) |
|---|---|---|
| Idle CPU over 30 s | 0 ms | 0 ms |
| Private memory, idle | 5.7 MB | 54 MB |
| Working set, idle | 15 to 26 MB | 88 MB |
| Threads / handles | 7 / 318 | 19 / 958 |
| CPU with the flyout open and sound playing, 10 s | below the counter resolution (16 ms) | not measured |
| Start until message loop is idle | about 55 ms | not measured |

Working set includes shared system DLL pages, so private memory is the fairer number.

## Not implemented yet

- Moving an individual app to a different output device (EarTrumpet does this through an undocumented, version-fragile interface).
- Global hotkeys, input devices (microphones), translations.
- Scrolling when more apps are playing than fit on screen: extra apps are cut off.

## Limitations

- A pinned value always wins. If you pin the speaker volume, the keyboard volume keys will be reverted until you forget the pin.
- Devices that only support coarse volume steps are matched within 1.5%.
- `IPolicyConfig`, used to switch the default device, is undocumented. It has been stable for many Windows releases but is not guaranteed.

## License

MIT
