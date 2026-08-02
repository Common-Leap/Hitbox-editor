# Visionary

Visionary is a desktop editor for viewing and editing most things about fighters in Super Smash Bros. Ultimate, except for models and animations. Changes are previewed in the running game through the included Skyline plugin.

## Components

- `src/` contains the desktop application.
- `plugins/slight_replica/` contains the in-game effect viewer and live-edit plugin.
- The upstream `ssbh_wgpu` crate provides character model, animation, skeleton,
  and weapon rendering.
- The [`effect_library`](https://crates.io/crates/effect_library) crate on
  crates.io provides `.eff` parsing, editing, and export.
- The effect editor exposes the documented emitter, particle, animation,
  rendering, sampler, shader, and spawn settings described by
  [EffectResearch](https://github.com/LilyLavender/EffectResearch).

## Prerequisites

- A recent Rust toolchain
- The `cargo-skyline` toolchain for building the in-game plugin

Cargo downloads the pinned `ssbh_wgpu` revision, `effect_library`, and the other
Rust dependencies automatically during the build.

## Game data

Use ArcExplorer to dump the `fighter` and `effect` folders from your
`data.arc`. The `data.arc` file is located in the game's RomFS.

Keep both dumped folders beneath the same export root:

```text
ArcExplorer export/
├── fighter/
└── effect/
```

Open this export root in Visionary when prompted. The `fighter` folder provides
character models, motion data, and parameters; the `effect` folder provides the
`.eff` files used by the effect editor.

## Desktop editor

Build Visionary from the repository root with the script for your platform.

Windows:

```bat
build.bat --release
```

Linux:

```bash
bash build.sh --release
```

The Linux script also registers the application launcher and scalable icon in
the current user's desktop environment. Set `VISIONARY_SKIP_DESKTOP_INSTALL=1`
to build without updating that launcher. For development, run Visionary with
`cargo run`. Visionary's application ID matches the installed desktop entry so
Wayland compositors can resolve the icon normally; the icon is also embedded in
the executable for native window decorations on Windows and X11.

Visionary reads the dumped data from its existing location and remembers the
selected export root for future sessions.

## Eden emulator setup

Visionary's live preview workflow uses the latest [Eden Nightly
build](https://eden-emu.dev/downloads/). On the download page, select the
**Nightly** channel and download the build for your operating system and CPU.
Eden Nightly changes frequently, so update to the newest build before
troubleshooting a connection problem.

Configure Eden's network interface before starting the game:

1. Open **Configure → System → Network** in Eden.
2. Set **Network Interface** to the active network card used by the computer,
   such as the connected Ethernet or Wi-Fi adapter. Do not leave the interface
   unselected.
3. Apply the setting, then start or restart the game.

The network-interface setting is required for the in-game plugin to expose its
connection to Visionary. The editor connects automatically when Eden and
Visionary are running on the same computer.

## In-game plugin

Build the Skyline plugin with the script for your platform.

Windows:

```bat
plugins\slight_replica\scripts\build.bat
```

Linux:

```bash
bash plugins/slight_replica/scripts/build.sh
```

The resulting `lib_effect_viewer.nro` is written beneath
`plugins/slight_replica/target/aarch64-skyline-switch/release/`. See the
[plugin guide](plugins/slight_replica/README.md) for deployment, runtime
dependencies, and live-edit setup.

Visionary finds standard Eden, yuzu, and Ryujinx SD-card locations through the
host operating system's application-data directories. For a portable or custom
emulator installation, set `VISIONARY_SD_DIR` to the emulator SD root before
starting Visionary. `VISIONARY_CACHE_DIR` can similarly move Visionary's cache
and temporary workspace to another location.

## Projects and mod exports

The **Mod** menu keeps hitbox, effect-spawn, authored effect, texture, and
transplant edits together:

- **Export Project** writes a portable `modproject.json`. If imported texture
  images are used, keep the generated asset folder beside the JSON file. These
  editable files are exported separately from mod and developer files.
- **Load Project** replaces the current project, restores every edit, and sends
  the available live rules and effects to a connected game. Added or retimed
  move events may ask you to perform that move once so the plugin can capture
  the original arguments safely.
- **Export Mod Folder** creates one complete ARCropolis mod directory. Copy that
  directory into `<SD root>/ultimate/mods/`. Rebuilt effects are under `effect/`,
  and the built ACMD plugin is chainloaded from `plugin.nro` at the root of the
  same mod.
- **Export Developer Files** writes rebuilt effect files to `effect_mod/` and
  the buildable Rust ACMD project to `acmd_source/`.

## Additional tools

Reusable game-analysis utilities are available in `research/decomp/ssbu-re/`.
Each tool reads its inputs from the external directory selected through
`SSBU_DUMP_DIR`.
