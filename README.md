# Visionary

Visionary is a desktop editor for viewing and editing most things about fighters in Super Smash Bros. Ultimate, except for models and animations. Changes are previewed in the running game through the included Skyline plugin.

## Components

- `src/` contains the desktop application.
- `plugins/slight_replica/` contains the in-game effect viewer and live-edit plugin.
- The upstream `ssbh_wgpu` crate provides character model, animation, skeleton,
  and weapon rendering.
- The [`effect_library`](https://crates.io/crates/effect_library) crate on
  crates.io provides `.eff` parsing, editing, and export.

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

Build or run Visionary from the repository root:

```bash
cargo build
cargo run
```

Visionary reads the dumped data from its existing location and remembers the
selected export root for future sessions.

## In-game plugin

Build the Skyline plugin with:

```bash
bash plugins/slight_replica/scripts/build.sh
```

The resulting `lib_effect_viewer.nro` is written beneath
`plugins/slight_replica/target/aarch64-skyline-switch/release/`. See the
[plugin guide](plugins/slight_replica/README.md) for deployment, runtime
dependencies, and live-edit setup.

## Additional tools

Reusable game-analysis utilities are available in `research/decomp/ssbu-re/`.
Each tool reads its inputs from the external directory selected through
`SSBU_DUMP_DIR`.
