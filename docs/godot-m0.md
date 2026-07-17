# Godot M0 development

Spurfire's game client is a **Godot 4.7.1 project with a Rust GDExtension**. M0 is deliberately a local, graybox movement slice: validate horse feel, camera, collisions, and telemetry before adding networking or content.

## Prerequisites

All platforms need:

- Rust 1.91 or newer, installed with `rustup`.
- Godot 4.7.1 (standard build, not the .NET build).
- [`just`](https://github.com/casey/just).
- Git and Bash. Windows users should run the shell scripts from Git Bash; WSL can build Linux artifacts but cannot produce the native Windows DLL without a configured cross toolchain.

### macOS

Install Xcode Command Line Tools (`xcode-select --install`), Rust, Godot 4.7.1, and `just`. Both Apple Silicon (`arm64`) and Intel (`x86_64`) are detected. Build and start the editor:

```sh
just game-build
just game-editor
```

Godot loads `game/bin/macos/libspurfire_godot.<profile>.<arch>.dylib` (for example, `libspurfire_godot.debug.arm64.dylib`). The Rust and Godot architectures must match.

### Linux

Install a C/C++ linker and Godot's desktop runtime libraries using the names provided by your distribution, then install Rust, Godot 4.7.1, and `just`:

```sh
just game-build
just game-test
```

The extension is copied to `game/bin/linux/libspurfire_godot.<profile>.<arch>.so`. CI exercises the x86_64 Linux path.

### Windows

Install Visual Studio 2022 Build Tools with **Desktop development with C++**, Rust's `stable-x86_64-pc-windows-msvc` toolchain, Godot 4.7.1, `just`, and Git for Windows. In Git Bash:

```sh
export GODOT_BIN='/c/Tools/Godot/Godot_v4.7.1-stable_win64.exe'
just game-build
just game-test
```

The extension is copied to `game/bin/windows/spurfire_godot.<profile>.<arch>.dll`. Do not mix the GNU and MSVC Rust toolchains. Native ARM64 Windows remains a platform-validation caveat until Godot, gdext, and packaging are exercised together on that target.

## Commands

- `just game-build` — debug build of `spurfire-gdext` and install the native library.
- `just game-build release` — optimized build and install.
- `just game-test` — bounded execution of `res://scenes/headless_smoke.tscn` with headless display and dummy audio drivers.
- `just game-editor` — open the project in Godot.
- `just game-run` — run the project.

Set `GODOT_BIN` when the executable is not named `godot4` or `godot`. Headless commands default to a 120-second limit; override it with `GODOT_TIMEOUT_SECONDS`. Build output under `game/bin/` is generated and gitignored. Never commit native libraries, import caches, editor state, or telemetry CSV files.

## Controls

- **W / left stick up:** move forward; automatically enters Walk from Idle.
- **S / left stick down:** brake, then reverse after stopping.
- **A/D / left stick:** steer left/right using Godot's visual yaw convention.
- **Shift/Ctrl / shoulder buttons:** increase/decrease gait.
- **Option (Alt) / left trigger:** hard brake.
- **Space / bottom face button:** jump.
- **R / Start:** reset to spawn.
- **Mouse / right stick:** orbit camera.
- **Escape once:** release mouse capture. **Escape again:** quit the prototype. Left-click recaptures.

## M0 contract

The native `HorseController` is a `CharacterBody3D` and owns the four-gait movement model, jump/coyote behavior, reset, rough terrain and slope response, and 10 Hz telemetry. Godot owns the graybox course, InputMap, camera, HUD, and smoke-test runner. The fixed physics rate is 60 Hz.

The measurable exit checks are:

- Standing start reaches at least 10.5 m/s within 3.5 seconds and 30 m.
- Hard braking from gallop stops within 20 m; coasting takes at least 50 m.
- Gallop turn radius stays in the 18–30 m band; walking supports a turn inside a 3 m circle.
- Every commanded gait transition signals within one frame; speed floors trigger automatic downshift.
- Jump clears the 1 m fence, including a jump inside the 0.15 s coyote window.
- Rough terrain reduces speed 25–35% and recovery reaches 95% within 2 seconds.
- The 25 degree slope preserves 55–75% flat speed; the 45 degree face is unclimbable.
- Reset converges within 0.5 seconds to spawn, zero velocity, upright Idle, with one Idle signal.
- Camera distance remains 4.5–6.5 m without clipping; stopped FOV reaches 70 within 0.5 seconds.
- Telemetry remains 10 Hz +/-1 Hz for a 60-second soak, with no gap over 150 ms and speed error under 5%.
- Ten course laps produce no collision pass-through or flat-ground snag below 1 m/s.
- Five idle seconds produce less than 0.1 m/s drift.

M0 contains no weapons, Saddle Dive, networking, external art, or non-graybox animation assets. **RustScale integration starts only after M0 movement validation.** The intended in-process Rust path avoids the current C ABI's missing gameplay UDP, but desktop/mobile/console packaging, telemetry gaps, peer-relay behavior, and all-platform support still require explicit validation before M6.

## CI and troubleshooting

The Linux game job pins Godot 4.7.1, verifies the downloaded archive with the release SHA-512 manifest, builds the debug extension, and runs the headless smoke scene. A failure to load the extension usually means the library is absent, has the wrong architecture, or has unresolved system libraries. Run `scripts/build-gdext.sh debug`, then launch Godot from a terminal to retain loader diagnostics.
