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

## Your own ACMD source

By default Visionary reads ACMD scripts from an online archive of the *vanilla*
scripts. If you have already modded a move, that is not the code your game runs.

Open **Windows → ACMD Source** and link the Rust project that builds your
plugin — the folder holding its `Cargo.toml`. Visionary indexes every
`unsafe extern "C" fn game_*` / `effect_*` function in it and reads the selected
move from there instead, so the editor shows the macros you actually called.
Both the smashline layout (`Agent::new("mario")` beside the scripts) and the
older `#[acmd_script(agent = "…", script = "…")]` attributes are recognised.

A project rarely overrides everything, and it does not have to. Each category is
resolved on its own: your `game_attackairn` is used for the hitboxes, and the
move's stock effects and sounds still come from the vanilla scripts, so a mod
that only retimes a hitbox does not make the move look silent and effectless.
Anything your project does define always wins over the vanilla copy.

The same window edits a script in place: pick **Hitboxes** or **Effects** to
open that function in a small text editor. Saving needs the function to exist in
your project — Visionary will not invent a place to put an effect you edited but
never wrote.

The editor and the rest of the app stay in sync both ways while you work:

- Typing in the source updates the timeline, the viewport, and the live game
  preview as soon as the text settles. Half-finished code is ignored rather than
  blanking the panels, and the last readable version stays on screen.
- Dragging a value in the editor panels writes it straight back into the source
  text, so the code always shows what you are looking at.

Nothing touches the file on disk until you press **Save**, which rewrites only
that one function and leaves the rest of the file alone. **Revert** restores the
script as loaded, panels included.

**Show** switches between your source, the code Visionary *would* write into
`acmd_source/` if you exported the move right now, and both side by side. The
generated pane runs the real export emitter on the move as it currently stands,
so it is exactly what you would get — not an approximation of it. If a spawn
cannot be exported under the macro your script used, it says which one and why.

The generated pane does not need a linked project, and works for any move the
editor has loaded — including one captured live from the game, which has no
script file anywhere. That case is the reason it is worth having.

### Checking the generated code

Under that pane is the result of the same check every export runs. It does not
ask the emitter what it meant to write. It reads the generated code back with the
parser the editor uses on your own scripts, and compares what comes out with the
move on screen — every field of every collision and spawn, by name.

Five things are checked. The first three stop an export rather than warn about
it, because a mod that does not build, or that quietly ships numbers other than
the ones you set, is worse than no mod:

- **It is Rust.** Every generated file is parsed. Lines kept verbatim from your
  script, and recorded macro tails, are spliced into the output as they are, so
  this is not a formality.
- **It says what you said.** A single rounded decimal is a failure. Exports used
  to write collision values to one decimal place, so a vanilla `0.35` hitbox
  attribute shipped as `0.3`, and a grab box at `-17.25` as `-17.2`, with nothing
  anywhere to say so.
- **It will build.** A value that is not a number, a graphic name with a quote in
  it, a wind command an argument short or one smashline never wrapped, two moves
  whose names differ only by punctuation and collapse onto one function —
  anything that produces a well-formed but broken mod is caught here rather than
  by your toolchain.
- **It is not wasteful.** A call issued twice in one block, an empty block, a
  `wait(0)`, a collision cleared before it comes out. These only inform.
- **It does not lose anything.** An effect script is regenerated from the calls
  Visionary understands, so a line it has no editor field for used to be dropped.
  Most are now copied through instead, in position, still inside whatever `if`
  they were written in — a costume-specific recolour stays specific to that
  costume. What is copied is listed too, because a copied line is not an
  understood one: editing the move around it will not update it, and it has to be
  valid Rust on its own. The few lines that genuinely cannot be kept are named
  individually, with the exact text and how many times it goes.

A branch of the move's own — an `if(WorkModule::is_flag(…)){`, its `else` — is
kept whole, condition and closing brace included, and the lines inside it stay
inside it. A hitbox written under one still shows on the timeline, because the
editor cannot know which way the branch will go and hiding it would be worse;
editing that hitbox rewrites it where it is.

The timing checks are skipped for a script carrying branches of its own, or an
`FT_MOTION_RATE`. Those decide at runtime what runs and when, the editor does
not model them, and a warning that guesses is worse than no warning at all.
Everything else is checked either way.

Write-back rewrites argument *values* only: the macros you called, your
comments, and your formatting all stay exactly as written. Every property the
hitbox and effect panels expose is covered — the masks, the sound and collision
attributes, the flags, the capsule endpoints — so an edit either lands in the
file or is named in the report under the editor. It is never dropped quietly.

Grab boxes are read and written as the `CATCH` calls they are, so a grab in your
script shows up on the timeline and can be retuned like any other collision. The
status kind and situation mask are not editable properties, and your own values
for them are carried through untouched.

A hitbox keeps the macro it was written as. `ATTACK` and `ATTACK_IGNORE_THROW`
carry the same arguments but are not the same call — the second one still hits a
fighter who is already being thrown — so the **Macro** dropdown in the hitbox
properties says which one this is, and exports and live previews fire the one you
picked. Switching between them is a change of macro rather than of value, so it
lands in an export but is reported when syncing into your own source.

Wind areas are written back too. The four `AREA_WIND_2ND` commands share their
first eight arguments and nothing else — the ninth is the rectangle's height and
the radial call's lifetime — so each is matched and retuned only against calls of
its own command, and a rectangular value can never land in a radial one. The
lifetime is an argument, so dragging it moves the timeline bar with it and lands
in the file; the shorter commands have no lifetime and run until an
`AreaModule::erase_wind`, whose frame is a different line and so is reported
rather than moved. Switching an area between rectangular and radial is a change
of command, not of value, so it lands in an export and is reported on source sync.

An effect's playback rate is the `LAST_EFFECT_SET_RATE` line beneath its spawn.
That macro names no effect — it changes whatever spawned last — so the rate is
shown as a property of the spawn above it and travels with that spawn when you
disable or move it. The **Rate** checkbox is the difference between no rate line
at all and one that happens to say 1.0; turning it on or off adds or removes a
call, so it lands in an export but is reported when syncing into your own
source, while changing an existing rate is written straight into the file. A
rate that does not sit directly beneath a spawn Visionary recognises is left
alone rather than attached to whichever spawn came before it.

**Tint** and **Opacity** are the `LAST_EFFECT_SET_COLOR` and
`LAST_EFFECT_SET_ALPHA` lines, and behave the same way: a colour picker and a
drag beside their own checkboxes, belonging to one spawn rather than to an
effect kind. The drag fields are not clamped to the picker's range, because the
vanilla scripts write colours brighter than white. If a live colour multiplier is
set on the same effect, that multiplier is what ships and the report says so —
rather than both being written and the second quietly winning, which is what
happened before.

These lines are also why a move can preview right and export wrong. Because they
name no effect, one sitting behind an `if` is separated from the spawn it
modifies, and Visionary will not guess which spawn that was. Nearly every vanilla
use is of exactly that kind — a costume check wrapped around a recolour — so
those are listed under the generated code as lines the export drops, not attached
to whichever spawn happens to be nearest.

Throws carry a hit with no volume. `ATTACK_ABS` sets the damage and knockback
applied to an opponent who is *already* caught, so it has no bone, no size, and
no position — the fields that would describe those are hidden rather than shown
as zeros, and it draws on the timeline but never in the viewport. What it does
have is the outcome it describes, shown as **Applies to**: a catch, a throw, or
a fighter-specific kind such as Terry's final smash. Two of these often sit in
one block naming the same id and differing only in that, so they are matched on
the kind and never on the id.

Intangibility is the **Hurtboxes** section under the collision list. `HIT_NODE`
and `HIT_NO` set how one bone or one numbered hurtbox group receives hits, and
`WHOLE_HIT` sets every bone at once — shown as **all bones**, with no target to
pick because the macro has none. The game holds that setting until something
takes it back, so a state is shown as the span between the call that sets it and
the call that restores it, rather than as two unrelated lines you have to pair up
by eye. `HIT_RESET_ALL` ends every one of them at once. Changing a status or a
bone is a value edit and is written into your source; adding or removing a call,
or moving one to a different frame, is structure and so lands in an export and is
reported when syncing.

A `WHOLE_HIT` span is tracked separately from the per-bone ones, so setting the
whole body translucent will not appear to end a `HIT_NODE` state that is open at
the same time. In the game it does cover those bones. No vanilla script mixes the
two — every use of `WHOLE_HIT` in the archive stands alone, in final-smash and
thrown states — so there was nothing to calibrate the interaction against, and
showing a span the script does not describe would be a worse guess than showing
none. If you mix them yourself, read the two as independent.

`COL_PRI` sits in the same section, and is the odd one out there. It ranks a
fighter's colour blend against the others applied at the same moment — it is a
`FLASH`-family command rather than a hurtbox one — and it is ended by
`COL_NORMAL`, not by `HIT_RESET_ALL`. They are separate resets of separate
things, so one is never used to close the other. It appears among the hurtboxes
because that is where a `game_` script's statements are edited; in an `effect_`
script the pair is edited with the colour commands below.

**Hitbox tuning** is the section below that, for the two commands that retune a
hitbox which is *already out*. `ATK_POWER` re-sets a box's damage and
`ATK_SET_SHIELD_SETOFF_MUL` scales the shield push-off it applies; each names the
hitbox id it acts on, and each runs on its own frame. That frame is the reason
they are their own rows rather than extra fields on the hitbox: a script may tune
a box in the same breath as creating it, or five frames later once it is already
hitting, and only a separate line can say the second. Editing the id or the value
is written into your source; moving one to another frame, or turning it into the
other command, is structure and lands in an export instead. A row shows a small
amber marker if it names an id this move never opens — the call is then retuning
nothing, which the script itself gives no sign of.

Two commands that look like they belong here do not. `ATK_HIT_ABS` and
`ATK_LERP_RATIO` name no hitbox id, so there is nothing for them to modify; every
vanilla `ATK_HIT_ABS` also passes local variables rather than values, so there is
nothing to edit either. Both are carried through an export exactly as written.

**Detection boxes** — `SEARCH` — are collisions that hit nothing. A search volume
has the same geometry a grab box does, and it is drawn in amber to keep that
distinction visible: it looks like a hitbox and cannot damage anyone. What it
*does* is report that something is inside it, and what the fighter does about
that lives in the status code, which no ACMD script can reach. Kirby's inhale is
the clearest example — a grab box, a detection box and the swallow's damage all
come out in the same frame, all three numbered 0.

Alongside the geometry you can edit what the volume looks for and which hurtbox
states count as found. Note that these use the `HIT_STATUS_MASK_*` constants,
which are a different set from the `HIT_STATUS_*` states in the hurtbox section
above and overlap them numerically — `HIT_STATUS_MASK_NORMAL` is 1, the same
number as `HIT_STATUS_INVINCIBLE`. They are different arguments to different
commands; the editor keeps them apart and so should you.

Nothing ends a search volume, so one has no end frame: there is no macro that
takes it back, and the two vanilla scripts that do close one close it from the
status code. A box therefore runs to the end of the move, which is what the
script actually says rather than a frame invented for the timeline.

`SEARCH` is written two ways in the game's own scripts — with and without its
three capsule-endpoint arguments — and four of the seven vanilla calls use the
shorter one. Adding a capsule to a call written without the slots needs three new
arguments rather than a changed one, so that edit is reported and lands in an
export instead of being written into your source. `SET_SEARCH_SIZE_EXIST`, which
re-sizes a box already out, is not modelled: it belongs with the hitbox tuning
above rather than here, and no vanilla script uses it.

**Sounds** get their own band at the bottom of the timeline, one row per call,
each a tick at the frame it fires on with the sound's name beside it. A move's
`sound_` script is read alongside its hitboxes and effects, so you can see the
swing that goes with a hitbox, the footsteps a run plays, and the landing thud
that lands well after the last hitbox closed — the timeline widens to fit that
last one rather than cutting it off. `STOP_SE` is drawn in a colder colour,
because it silences a sound rather than playing one.

A sound is a tick and not a bar on purpose: the script says when one starts and
nothing in ACMD says when it ends. Sounds are shown but not yet editable, and
nothing about them is written or played back — reading them changes only what
you can see.

`FLASH`, `COL_NORMAL` and the `BURN_COLOR` family tint the fighter's model or the screen
flash. They sit in the effect list beside the spawns, but they are not spawns:
there is no graphic, joint, or position, so those fields are hidden and a colour
picker takes their place. **Add colour command** creates one. The vanilla scripts
almost always use them in pairs — one call that snaps to a colour, and a `_FRAME`
one directly after that fades the blend in or out over a number of frames — and
both halves are editable. Changing values writes them into your source; switching
which command a call is changes how many arguments it takes, so that lands in an
export and is reported when syncing.

Three of them take no arguments at all — `COL_NORMAL`, `BURN_COLOR_NORMAL` and
`START_INFO_FLASH_EYE` are resets, and there is nothing in them to change. They
still appear in the list, because where a reset sits is the whole of what it
does: dragging it later leaves the tint on screen for longer, and disabling it
leaves the tint on for good.

Before this they were dropped: the effect export regenerates the whole function
from the calls it knows about, so every exported move lost its colouring without
saying so.

A spawn written inside a condition keeps it. Several fighters spawn one graphic
facing left and a different one facing right, or recolour an effect once per
costume; the export reproduces the check around the call rather than flattening
it, so the move still does one thing at a time instead of all of them at once.
Lines the editor has no field for ride along beside the call they followed, which
matters because a `LAST_EFFECT_SET_COLOR` recolours whatever spawned last — moved
even a little, it lands on the wrong effect. The effect panel shows these under
**Kept as written**.

Anything that cannot be written as a value change to an existing argument is
reported instead of guessed at: a spawn you added or removed, a graphic you
renamed, a retimed call, one iteration of a `for` loop edited on its own, or a
sword trail's position — a trail is drawn between the joints it names and has no
transform arguments at all. **Sync Edits Into Source**, in the **Mod** menu,
applies the same write-back to the file directly, for when the source window is
not open.

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
