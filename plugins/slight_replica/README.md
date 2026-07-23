# Visionary in-game plugin

`slight_replica` is Visionary's Skyline plugin for Super Smash Bros. Ultimate.
It runs inside the game, reports live effects and ACMD activity to the desktop
editor, and applies hitbox and effect edits while the game is running.

## Requirements

- The Visionary source
- A recent Rust toolchain for the desktop editor
- The Rust Skyline toolchain and `cargo-skyline` for the plugin
- A Skyline-compatible Super Smash Bros. Ultimate setup
- Arcropolis, NRO Hook, and the Smashline 2 runtime
- An ArcExplorer export containing sibling `fighter` and `effect` folders

The required runtime NROs are:

| Plugin | Role |
| --- | --- |
| `libarcropolis.nro` | Loads the mod's RomFS files |
| `libnro_hook.nro` | Provides the NRO hooks required by Smashline 2 |
| `libsmashline_plugin.nro` | Provides the Smashline 2 callback runtime |
| `lib_effect_viewer.nro` | Connects the running game to Visionary |

The installed Smashline runtime must export
`smashline_install_state_callback` and
`smashline_install_line_callback`. The plugin will not receive the callbacks it
needs when Smashline is missing or incompatible.

## Build Visionary and the plugin

Run the following commands from the Visionary repository root:

```bash
cargo build --release
bash plugins/slight_replica/scripts/build.sh
```

The editor is produced in the root Cargo release output. The plugin build writes
`lib_effect_viewer.nro` to the plugin's Cargo release output and copies it to the
plugin's `target/output` directory.

## Install the plugin

Use the mod directory configured by the emulator or console instead of copying a
machine-specific path from another installation.

1. Make sure the Skyline loader files are installed in the mod's ExeFS.
2. Open the Arcropolis mod's `romfs/skyline/plugins` directory.
3. Place the four runtime NROs listed above in that directory.
4. Remove an older `libeffect_viewer.nro` if one is present. Loading both names
   would install the hooks twice.
5. Copy the newly built `lib_effect_viewer.nro` into the directory.
6. Fully restart the game or emulator so Skyline loads the new plugin.

The deployment scripts under `scripts/` are convenience helpers for standard
Eden and Ryujinx installations. Review them before use because emulator data
directories are configurable and differ across operating systems. Manual
installation is the portable option.

## Prepare the game data

Use ArcExplorer to export the `fighter` and `effect` folders from the game's
`data.arc`. Keep them under one export root:

```text
ArcExplorer export/
├── fighter/
└── effect/
```

Start Visionary and select that export root when prompted. Visionary reads the
files in place and remembers the selected location.

## Connect Visionary

The plugin listens for the desktop editor on TCP port `7878`. Visionary connects
to `127.0.0.1:7878` automatically when its live editing interface is opened.

For an emulator running on the same computer:

1. Launch the game with the plugin installed.
2. Enter a match or Training Mode so game-frame callbacks begin.
3. Start Visionary and open the Effects or live hitbox interface.
4. Confirm that Visionary reports `game connected`.
5. Perform an action in-game to populate live effects and ACMD captures.

No Rust Parameter Manager or FTP server is required for this normal Visionary
workflow.

For a physical console or an emulator on another computer, forward the plugin's
TCP port to port `7878` on the computer running Visionary. The current editor
connects to localhost, so the forwarding endpoint must appear locally at
`127.0.0.1:7878`.

## Optional Rust Parameter Manager compatibility

The plugin retains the SLight/Rust Parameter Manager protocol for users who need
that workflow. RPM connects to the same TCP server as Visionary, and only one
interactive client should be used at a time.

For an emulator, RPM also needs FTP access to the emulator's virtual SD card:

1. Install the FTP helper dependency:

   ```bash
   python3 -m pip install --user pyftpdlib
   ```

2. Prepare the SLight directories in the emulator's configured SD-card root:

   ```bash
   python3 plugins/slight_replica/tools/setup_eden_sd.py \
     --sdmc "<emulator SD root>" \
     --host 127.0.0.1 \
     --port 7878
   ```

3. Start the FTP bridge against that same SD-card root:

   ```bash
   python3 plugins/slight_replica/tools/ftp_server.py \
     --root "<emulator SD root>"
   ```

4. Configure RPM to connect to the game on TCP port `7878`. Configure its FTP
   address as `<host>:5000/slight/user/debuggables` and use the username and
   password printed by the FTP bridge.

A physical console can provide the same SD-card access with `sys-ftpd`, so the
host-side FTP bridge is not needed there.

The plugin creates its SLight runtime directories automatically. The optional
setup helper writes `gateway.txt`, whose port value changes the plugin's listen
port. Keep port `7878` when using Visionary because the editor currently expects
that port.

## Troubleshooting

- If Visionary remains offline, confirm that the game has reached a mode where
  frames are advancing and that TCP port `7878` is reachable.
- If the plugin does not start, verify the Skyline loader and all three runtime
  dependencies, then fully restart the game.
- If live effects never appear, verify that the installed Smashline plugin is
  Smashline 2 and exports the required callback symbols.
- If hooks run twice or the game becomes unstable, remove the legacy
  `libeffect_viewer.nro` name and leave only `lib_effect_viewer.nro`.
- Runtime diagnostics and saved edits are stored in the `slight` directory on
  the emulated or physical SD card.

## Project layout

```text
src/
  lib.rs                          plugin entry point
  slight/
    frame_context.rs              match and current-agent state
    systems/                      runtime system implementations
    main_smash/                   system installation and frame dispatch
    effect_viewer/                effect tracking and live editing
    hitbox_viewer/                ACMD capture and hitbox rules
    agent_extender/               agent lifecycle integration
    agents.rs                     live fighter and weapon registry
  rust_extender/
    net/simple_server.rs          TCP transport
    debugging/debuggable_server/  Visionary/RPM objects and transactions
scripts/                          build and deployment helpers
tools/                            optional RPM and emulator utilities
```
