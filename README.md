# Arc

A lightweight dynamic-island-style overlay for Windows: a frameless, always-on-top
pill flush with the top edge of the screen that expands into panels for media,
timer, calendar, battery, network, volume, clipboard history, downloads and more.

Written in Rust against Win32 / DirectComposition / Direct3D 11 / Direct2D /
DirectWrite - no webview, no XAML, no runtime to install.

## Status

In development. The window, rendering, spring animation and scene graph are
working; services and panels are being filled in module by module. The design
notes and backlog live in the numbered docs.

## Design in one paragraph

`platform` (Win32 + DComp swapchain, DPI, tray, drag-drop) -> `core` (scene graph,
spring integrator, geometry, colour, image cache) -> `services` (SMTC media, PDH
metrics, WASAPI audio, brightness, power, clipboard, downloads) -> `ui` (pure
layout of state into scene nodes) -> `app` (HUD state machine, timers). Only
`platform` touches Win32; `ui` is a pure function of state, so panels stay
testable.

## Build

```bash
git clone https://github.com/ErfanBagheri404/Arc.git
cd Arc
```

```bash
cargo build --release      # target/release/arc.exe
cargo test
```

Requires Rust 1.75+ with the MSVC target; Windows 10 1809 or newer.

## Docs

| file | contents |
|---|---|
| `00-INDEX.md` | decisions and doc map |
| `01-ATOLL-TEARDOWN.md` | prior-art teardown |
| `02-WINDOWS-LANDSCAPE.md` | Win32 overlay constraints |
| `03-FEATURE-SCOPE.md` | feature scope |
| `04-TECH-STACK.md` | stack and why |
| `05-ARCHITECTURE.md` | layer diagram |
| `06-PLAN.md`, `08-BACKLOG.md` | milestones and open work |
| `07-RISKS.md` | known risks |

## Layout

| path | role |
|---|---|
| `src/platform/` | window, D3D/D2D renderer, DPI, tray, settings window |
| `src/core/` | scene, animation, geometry, colour, images |
| `src/services/` | OS-facing data providers |
| `src/ui/` | panels: media, timer, calendar, listen, shelf, stats, settings |
| `src/app/` | HUD state machine |
