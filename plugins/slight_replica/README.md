# Visionary in-game plugin

`slight_replica` is Visionary's Skyline plugin for Super Smash Bros. Ultimate.
It runs inside the game, reports live effects and ACMD activity to the desktop
editor, and applies hitbox and effect edits while the game is running.

## Requirements

- The Visionary source
- A recent Rust toolchain for the desktop editor
- The Rust Skyline toolchain and `cargo-skyline` for the plugin
- Python 3 for the portable build, deployment, and optional FTP helpers
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

Build the desktop editor from the Visionary repository root:

```console
cargo build --release
```

Then build the plugin with the launcher for your platform.

Windows:

```bat
plugins\slight_replica\scripts\build.bat
```

Linux:

```bash
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

Skyline loads every file in the plugins directory, not only the names listed
above. A backup kept beside the plugin — `lib_effect_viewer.nro.bak`, a renamed
copy, an older build — is loaded as a second plugin, so the hooks run twice and
the frame rate halves. Keep spare builds somewhere else entirely.

The deployment helper below refuses to deploy while a second copy is present and
names the file, so following it is enough to avoid this. Two things catch it
after the fact, for a directory that was populated by hand:

- `diag.txt` opens with `!!!! SLIGHT: PORT ALREADY BOUND`. Only the copy that
  loses the race for the port can see this, so it appears once no matter how
  many copies are loaded.
- `SRV thread entered` appears once per loaded copy. Two of them is the proof.

The build recorded at the top of `diag.txt` is written by whichever copy loads
last, so it can name a build other than the one just deployed. That is a symptom
of the same problem, not evidence the deployment failed.

The cross-platform deployment helper uses the normal application-data location
for the selected emulator. A portable or customized install can declare its mod
root explicitly:

```console
python plugins/slight_replica/tools/deploy_plugin.py --emulator eden --mod-dir "<Arcropolis mod root>"
python plugins/slight_replica/tools/deploy_plugin.py --emulator ryujinx --mod-dir "<Arcropolis mod root>"
```

On Windows, `py -3` can be used in place of `python`. The same locations can be
declared once through `VISIONARY_EDEN_MOD_DIR` and
`VISIONARY_RYUJINX_MOD_DIR`. When deploying to Ryujinx, pass
`--source-mod-dir "<existing Skyline mod root>"` if the Skyline ExeFS and runtime
NROs should be copied from another installation. Manual installation remains
available for any custom layout.

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

When Visionary is not connected, deduplicated ACMD captures are still preserved in
`sd:/slight/user/acmd_captures.jsonl`. The archive is written by a background
worker and is not capped by the plugin's live-memory delivery queue. When the
editor connects later, the archive is replayed in small batches; reconnecting
does not create duplicate capture entries in the editor.

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

   ```console
   python -m pip install --user pyftpdlib
   ```

2. Prepare the SLight directories in the emulator's configured SD-card root:

   ```console
   python plugins/slight_replica/tools/setup_eden_sd.py \
     --sdmc "<emulator SD root>" \
     --host 127.0.0.1 \
     --port 7878
   ```

3. Start the FTP bridge against that same SD-card root:

   ```console
   python plugins/slight_replica/tools/ftp_server.py \
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
- If hooks run twice, the frame rate halves, or the game becomes unstable, leave
  exactly one copy of the plugin in the directory: no legacy
  `libeffect_viewer.nro`, and no backups or renamed builds beside it.
- Runtime diagnostics and saved edits are stored in the `slight` directory on
  the emulated or physical SD card.

## Effect-loader trace

The plugin can log the game's own effect resource loading — directory loads,
`load_effects` results, effect-set construction and readiness — to a set of
`effect_viewer_*.txt` files on the SD card. Those hooks run on the game's
loading thread, underneath Arcropolis, and each logged line is a separate file
write, so the trace is off by default. It is worth the cost only when
investigating a specific load.

Create `sd:/slight/debug/trace.txt` before starting the game to enable it, and
delete the file to turn it back off. With the trace off, the plugin's own log is
`sd:/slight/diag.txt`, which is buffered and written from the frame path.

## Bisecting a load hang

The plugin installs seven independent groups of hooks, several of them inside the
game's resource loader. When one of them wedges a match load there is nothing to
read afterwards: `diag.txt` is only flushed by the per-frame driver, which never
runs if the match never starts.

Naming a group in `sd:/slight/debug/off.txt` before the game starts leaves it
uninstalled for that boot, so a hang can be narrowed down a reboot at a time
instead of a rebuild at a time. Names are separated by anything non-alphanumeric,
so one per line or comma-separated both work:

| Name | Left uninstalled |
| --- | --- |
| `reload` | the effect-manager `load_effects` / `unload_effects` hooks |
| `liveeff` | the editor's merged-eff manifest registration |
| `effect` | the seventeen `EffectModule` request and kill hooks |
| `acmd` | the ACMD capture and injection hooks |
| `hitbox` | live ACMD capture and injection for hitboxes and sounds |
| `agent` | the Smashline line callbacks that drive the per-frame engine |
| `systems` | the SLight system facades and the editor's TCP server |

Inside the `effect` group the parts can be named separately: `carrier` for the
carrier-proxy redirection, `remap` for the transplant alias lookup, and `track`
for the whole spawn-tracking body. Individual `EffectModule` hooks go by
`req`, `req2d`, `reqfollow`, `reqonjoint`, `reqemit`, `reqcommon`,
`reqcontinual`, `reqtime`, `reqtimefollow`, `kill`, `endkind`, `detachkind`,
`killall`, `remove`, `removecommon` and `removetime`, with `reqs` and `kills`
covering each family at once. `killpass` reduces the stop-kind hooks to a bare
call through to the game, and `fanout` stops them re-issuing that call for the
aliased kind and for the carrier.

`EffectModule::kill_kind` is not in that list because it is not hooked at all.
Hooking it deadlocks match loading for any moveset that depends on One Slot
Effects; the reasoning is recorded where the hook used to be, in
`effect_viewer/mod.rs`.

Disabling `agent` or `systems` stops the per-frame engine, so live editing and
the editor connection go with it. Every boot records what it actually installed
in `sd:/slight/user/error_logs/effect_viewer_boot.txt`.

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
