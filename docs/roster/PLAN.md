# Roster & Character Authoring — Design

Status: core roster implemented (Phases 0–6 done); fully live CSS is Phase 7 and
blocked on reverse engineering. The task board is [`TODO.md`](TODO.md); every
item referenced below as `R-##` lives there.

## What this adds

One cohesive area of Visionary, called **Roster**, that covers the parts of a
fighter that exist *outside* a single move:

1. **Mod library** — import many compiled mods at once and see, per fighter and
   per slot, what each one provides and where they collide.
2. **CSS preview and layout** — a rendered Character Select Screen, with modded
   entries in place, that can be reordered and extended by direct manipulation.
3. **New entries** — add a character to the CSS as a slot-backed clone of a
   donor fighter, with a scaffolded home for the user's own model, animations,
   and moveset.
4. **Traits** — a typed editor for a fighter's `.prc` values (weight, gravity,
   run speed, air acceleration, jump counts, and the rest of `fighter_param`).

Everything it produces flows into the same three destinations the rest of
Visionary already targets: the in-memory project, the exported mod folder and
Smashline source, and the live game.

## The one architectural decision already made

**New characters are slot-backed clones, not new fighter IDs.**

A new roster entry is a costume slot on a donor fighter (`mario/cXX`), promoted
to its own CSS cell. It carries its own model, motion, effects, ACMD, and — where
the engine allows slot scoping — its own params. It does *not* get a new engine
fighter ID.

Why: it works on stock ARCropolis with no ID-expansion runtime, it composes with
every mod already in the ecosystem, and Visionary's `FighterEntry` model already
carries `slots: Vec<u8>` and slot-scoped edits (`TransplantOp::one_slot_slots`).
The cost is that anything the engine reads per *fighter* rather than per *slot*
stays shared with the donor. `R-40` is the audit that establishes exactly which
trait fields those are, and the trait editor must mark them as shared rather
than silently write a value that will not take effect.

Design consequence: **nothing in the data model may assume slot-backing.** A
roster entry is identified by an opaque `RosterEntryId` and resolves to a
`RosterBacking` enum whose only variant today is `SlotClone`. A future
`NewFighterId` variant must be addable without touching the CSS editor, the
trait editor, or the project format.

## Where this sits in the existing codebase

Visionary today is a *move* editor: pick a fighter, pick a move, edit ACMD and
effects, journal the edit into `ModProjectFile`, export or push live. The
Roster area is the first feature that operates above the move level, and the
first that touches game files outside `fighter/` and `effect/`.

Existing pieces it builds on:

| Piece | File | Role here |
|---|---|---|
| `AppState.fighters` / `FighterEntry` | `src/data.rs` | Already indexes fighters and their real on-disk slots from the data root and from extra mod roots. Becomes the raw input to the roster index. |
| `extra_roots` + `add_mod_root` | `src/app.rs` | The current one-folder-at-a-time mod import. The mod library supersedes its UI but keeps its persisted config file readable (`R-02`). |
| `ModProjectFile` | `src/mod_project.rs` | The serialized project. Gains `roster` and per-fighter `params`; version bumps to 3 with a migration (`R-05`). |
| `mod_export.rs` | `src/mod_export.rs` | Mod-folder and Smashline source emission. Gains UI-file and param emission. |
| `game_link.rs` ↔ `slight_replica` | `src/game_link.rs`, `plugins/` | The live channel. Gains new message kinds for param pokes and roster reloads. |
| `prc` (`prc-rs`) | dependency | Already read *and* written (`prc::save`) for hit-data params. The same crate covers `ui_chara_db.prc` and `fighter_param.prc`. |
| `param_labels.rs` | `src/param_labels.rs` | Already fetches and caches hash40→label from `ultimate-research/param-labels`. This is what makes a *typed, named* trait editor possible without hand-maintaining a field list. |

New file surface it needs. `src/app.rs` is already 42k lines; the Roster area
must not land in it. Everything new goes in a `src/roster/` module:

```
src/roster/
  mod.rs            — key space, RosterEntry, RosterBacking
  window.rs         — the Roster viewport and its four tabs
  library.rs        — mod import, root detection, manifest, conflict resolution
  archive.rs        — .zip / .7z extraction into the library cache
  index.rs          — merge data root + mod library + project into one roster view
  css.rs            — ui_chara_db read/write and the disp_order model
  css_view.rs       — the select-screen preview and drag-to-reorder
  icons.rs          — portrait discovery, decode, and the per-frame decode budget
  names.rs          — .xmsbt display-name overrides
  new_character.rs  — the new-character wizard and readiness panel
  scaffold.rs       — slot creation, costume gating, animation binding
  traits.rs         — fighter_param model and curated schema
  traits_view.rs    — the trait editor UI
  live.rs           — writing roster and values into the game's mod folder
  export.rs         — roster, values, and authored files into the mod folder
```

`src/app.rs` gains only the tab entry point and the state field. Enforced by
`R-70`.

## Data model

### Roster index

```rust
/// Stable across a session; NOT stable across restarts unless it is a
/// project-authored entry, which carries its persisted id.
pub struct RosterEntryId(pub u32);

pub struct RosterEntry {
    pub id: RosterEntryId,
    pub backing: RosterBacking,
    /// The engine fighter this entry ultimately plays as.
    pub fighter: String,
    pub display_name: String,
    /// Position on the CSS. Authoritative source is ui_chara_db; see css.rs.
    pub css_order: Option<u32>,
    /// Which imported mods and which roots contribute files to this entry.
    pub providers: Vec<ProviderId>,
    pub origin: EntryOrigin,   // Vanilla | Imported | Authored
}

pub enum RosterBacking {
    /// The whole fighter is the entry (vanilla and whole-fighter mods).
    Fighter,
    /// One costume slot promoted to its own CSS cell.
    SlotClone { donor: String, slot: u8 },
    // NewFighterId { .. } — deliberately absent; see the decision above.
}
```

The index is **derived**, never hand-maintained: `RosterIndex::build` takes the
data root, the mod library, and the open project, and produces the entry list.
Any UI that needs to know "what is on the roster" reads the index. Any edit
writes to the *project*, then rebuilds the index. This is the same discipline
`AppState.sounds` follows against `sound_script` — one owner, views refreshed
from it — and it is the single rule that keeps the CSS preview from drifting
away from what will actually be exported.

### Project additions

`ModProjectFile` gains, at version 3:

```rust
pub struct ModProjectFile {
    // ... existing ...
    #[serde(default)]
    pub roster: RosterMod,          // new
}

pub struct RosterMod {
    /// CSS position overrides: entry key → desired order. Sparse.
    pub order: BTreeMap<RosterKey, u32>,
    /// Display-name overrides.
    pub names: BTreeMap<RosterKey, String>,
    /// Entries this project created from nothing.
    pub authored: Vec<AuthoredEntry>,
    /// Hidden/removed CSS entries.
    pub hidden: BTreeSet<RosterKey>,
}
```

and `FighterMod` gains `params: ParamMod` — a **sparse** map of
`(param file, dotted path) → value`, not a whole-file copy. Sparse matters for
the same reason the ACMD edit log is sparse: two mods editing different fields
of `fighter_param.prc` must merge, and a whole-file copy makes that impossible.

`RosterKey` is a stable string (`"mario"`, `"mario#c08"`), not a session id, so
a saved project reopens onto a rescanned roster correctly.

## The four features

### 1. Mod library (`R-01` … `R-12`)

Today: `add_mod_root` appends one folder to `extra_roots`, and fighters found
under it are tagged `FighterSource::ModRoot`. There is no notion of a *mod* as a
unit, no conflict detection, and no way to disable one.

The library introduces `ImportedMod`: a name, a root path, an enabled flag, a
load order, and a scanned **manifest** of every game path it provides. Import
accepts a folder, a `.zip`/`.7z`, or a multi-select of either, and normalizes
each to a root directory under a stable cache path (per the global temp-dir
rule: `~/.cache/builds/visionary/mods/<slug>/`, reused and overwritten, never
`/tmp` and never a fresh dir per run).

Scanning must recognize that a compiled mod's root is not always the arc root —
mods commonly ship as `MyMod/fighter/...` or with a `ARC/` or `romfs/` wrapper.
`R-04` is the root-detection heuristic, and it must report what it chose rather
than guessing silently.

Conflict detection is the point of the manifest: when two enabled mods provide
the same game path, that is a conflict, and load order decides the winner. The
library shows conflicts per file and per fighter, and the roster index resolves
through load order. A mod that ships a `.nro` plugin is flagged — Visionary
cannot merge compiled plugin behavior, and the user needs to know that mod's
moveset changes are invisible to the editor (`R-11`).

### 2. CSS preview and layout (`R-20` … `R-33`)

The CSS is not in `fighter/` or `effect/`, so this is the first feature that
needs data outside the current export root. It needs:

- `ui/param/database/ui_chara_db.prc` — the roster database: one entry per
  character with its UI id, series, and the fields that drive CSS placement.
- `ui/message/msg_name.msbt` — display names.
- `ui/replace/chara/chara_2/...` — the CSS portrait images (bntx).

#### R-20 (answered) — `disp_order` is the CSS position

Verified against a real `ui/param/database/ui_chara_db.prc` (121 `db_root`
entries, 75 fields each). The two candidate fields are `disp_order` and
`save_no`, both `I8`, and they are identical for 110 of 121 entries — which is
exactly why this had to be measured rather than assumed. They diverge on the
Ultimate-era additions, and the divergence settles it:

| `name_id` | `save_no` | `disp_order` |
|---|---|---|
| bayonetta | 63 | 63 |
| richter | 64 | **67** |
| inkling | 65 | **64** |
| ridley | 66 | **65** |
| krool | 67 | **68** |
| simon | 68 | **66** |

The real character select screen runs Bayonetta → Inkling → Ridley → Simon →
Richter → K. Rool. That is `disp_order` sorted ascending. `save_no` is the save
slot / fighter number, and following it would place Richter before Simon and
K. Rool before both.

Everything else that matters about the field:

- **`-1` means "not on the select screen."** Bosses, the individual Pokémon
  Trainer Pokémon, and the unselectable Pyra/Mythra variants all carry `-1`,
  and every one of them also has `can_select = false`. The editor treats `-1`
  and `can_select = false` as the same state and writes both together; writing
  one without the other produces an entry the game disagrees with itself about.
- **`99` is the Random slot** (`name_id = "random"`, `can_select = false`). It
  is not a position in the sequence and must not be renumbered.
- **`disp_order` is not unique.** `eflame_first` and `elight_first` both hold
  `80` — Pyra and Mythra share one cell. Any "reorder" operation must therefore
  be able to move a *group* of entries sharing a value, and must not assume it
  can key entries by position.
- **The field is `I8`, so positions run 0–127**, with 99 reserved. Vanilla uses
  0–86. That is the hard ceiling on roster size through this field, and the new
  entry wizard has to check it rather than silently wrap into a negative.

The list order of `db_root` is *not* the CSS order — it follows `save_no` — so
the editor sorts by `disp_order` and leaves the list order alone.

#### R-21 (answered, with its limit stated) — the grid is a reflow of a linear sequence

`disp_order` is a linear index; the cell geometry lives in the character-select
layout under `ui/layout/`, not in any `.prc`, and this repository has no `ui/`
dump to verify column counts against.

So the preview renders the `disp_order` sequence reflowed into a grid at a
user-chosen column count, and **says so** — the column count is a display
convenience, not a claim about the game's layout. What the preview *is*
authoritative about is the thing being edited: the order of the sequence, which
is exactly what `disp_order` stores. Building a pixel-accurate replica of the
in-game grid would require the layout files and would add no editing power over
the sequence view.

If a `ui/` dump becomes available later, tightening the preview geometry is an
additive change behind the same ordering model.

The preview itself is an egui grid of portrait cells, drag-to-reorder, with
modded and authored entries visually distinguished from vanilla, and a diff
overlay showing what moved relative to the unedited roster. Portraits come from
the mod library where a mod provides one and from the game root otherwise;
`texture_import.rs` already decodes and encodes bntx, so icon handling reuses it
rather than adding an image path (`R-25`).

Writing back means emitting a modified `ui_chara_db.prc` into the mod folder.
Because the project stores a *sparse* order override rather than a file copy,
the emitted file is always rebuilt from the current game/library state plus the
override — so a project stays correct when the underlying mods change.

### 3. New entries and moveset scaffolding (`R-40` … `R-58`)

#### What a slot-backed character can and cannot get (measured)

The plan originally assumed a slot-backed character could be given its own cell
on the select screen by adding a `ui_chara_db` row that points its costumes at
the donor's higher slots. That assumption was checked against the real database
and **it is wrong**.

The candidate fields are `c00_index`…`c07_index` and `c00_group`…`c07_group`.
Across 121 vanilla rows, 25 have a non-zero index and 17 a non-zero group, and
the pattern says what they are:

| `name_id` | `cNN_index` |
|---|---|
| wario | `[0,1,0,1,0,1,0,1]` |
| ike | `[0,1,0,1,0,1,0,1]` |
| ice_climber | `[0,0,0,0,4,4,4,4]` |
| pikmin | `[0,0,0,0,4,4,4,4]` |

Wario's two outfits alternate across his eight slots; Ike's two designs do the
same; the Ice Climbers' last four slots swap which climber leads. These fields
group costumes into **variants**. They are not a redirect from one character's
costume list to another's, and no other field in the row is either.

So a new `ui_chara_db` row with the donor's `fighter_kind` would produce a cell
that launches the donor with *whatever costume the player picks* — not pinned to
the authored slot. Pinning it requires hooking costume selection at runtime,
which is plugin work in `slight_replica`, not a file edit.

**What the slot-clone strategy therefore delivers, all of it verified:**

- the character's own model and animations, in its own costume slot;
- its own moveset, through slot-gated ACMD (`R-50`);
- its own display name, because `msg_name` labels are per slot
  (`nam_chr1_<slot>_<name_id>`);
- full editability in Visionary's existing move editor.

**What it does not deliver:** a separate cell on the character select grid. The
character is selected as a costume of the donor.

`R-41` is scoped to that reality and the wizard says so up front. `R-55` is added
to the board for the runtime pin, marked blocked, with the finding recorded so
whoever picks it up starts from evidence rather than repeating this measurement.

#### The scaffold

Creating an entry is a scaffold operation, and the scaffold is the contract with
the user: it defines exactly where their model and animations go, so the rest of
the tool can find them.

`scaffold.rs` creates, under the project's mod folder:

```
fighter/<donor>/model/body/c<NN>/        ← drop your model here
fighter/<donor>/motion/body/c<NN>/       ← drop your animations here
effect/fighter/<donor>/ef_<donor>_c<NN>.eff
ui/...                                    ← portrait + name entries
```

plus a project-side `AuthoredEntry` recording the donor, slot, display name, and
the moveset scaffold. It does **not** copy the donor's model or animations —
copying produces a working-but-identical clone that hides whether the user's own
files were picked up. It creates the directories, writes a `README` naming the
expected filenames, and the roster panel shows per-entry readiness ("model:
missing, motion: 4 files, effect: stub").

The moveset is gated rather than scaffolded. A costume-backed character is
already playable the moment its model is in place, because it runs the donor's
moveset until a move is replaced — so generating copies of the donor's scripts
would add 24 functions that do exactly what their absence does. What the export
generates instead, per replaced move, is a costume gate: our costume runs the
authored body, every other costume runs the fighter's own. From there, **the existing move editor is the
moveset editor**: the authored entry appears in the fighter list, selecting a
move opens the normal Collisions/Hurtboxes/Motion/Sound/Effects tabs, and edits
journal into the same `FighterMod`. This is the "one unified cohesive tool"
requirement, and it is met by *not* building a second editor — the roster
feature's job ends at making the entry selectable.

`R-50` is the one that makes this real and must not be skipped: a created ACMD
function with no `agent.acmd` registration line compiles, installs nothing, and
plays vanilla. The scaffold must emit registration alongside every generated
script, and the readiness panel must report registration status, not file
existence.

### 4. Trait editor (`R-60` … `R-68`)

#### R-40 (answered) — every trait value is fighter-wide. None is per-slot.

Measured against the real dump. The traits live in **one shared file**,
`fighter/common/param/fighter_param.prc`, which holds a `fighter_param_table`
list of 94 structs. Each struct has **369 fields** — `weight`, `air_accel_y`,
`walk_speed_max`, `dash_speed`, `run_speed_max`, `jump_squat_frame`, `jump_y`,
`shield_radius`, `landing_attack_air_frame_*`, `jostle_*`, `attack_combo_max`,
and so on — and is identified by exactly one key: `fighter_kind`.

**Zero** of those 369 field names mention a slot, a colour, or a costume. There
is no per-slot dimension in the file at all. The per-fighter `vl.prc` (hurtbox
capsules, ledge-grab boxes, jostle collision) is likewise keyed by nothing below
the fighter.

The consequence for the slot-clone strategy is total rather than partial, and the
UI has to say so plainly:

> A slot-backed character shares **all** of its trait values with its donor.
> Changing its weight changes the donor's weight, and every other costume of that
> donor, in the same match.

This is the real cost of the slot-clone decision, and it is worth stating in one
place rather than as a per-field marker. A per-field "shared" badge — which is
what `R-67` was originally scoped as — would imply that some fields are *not*
shared. None are. So `R-67` becomes a single prominent notice on the trait editor
whenever the selected entry is a `SlotClone`, which is both accurate and harder
to miss.

It also relocates the file: `fighter_param.prc` is not under `fighter/<name>/`,
so `ParamMod` keys on the **game-relative** path of the file plus a dotted path
scoped to the selected fighter's row within it.

#### The editor

`fighter/common/param/fighter_param.prc` holds weight, gravity, speeds, jump
counts, shield values, and several hundred more fields, one row per fighter. The
editor:

- reads them with `prc`, names them with the already-cached `param_labels` map,
- groups them into human sections (Movement, Jumps, Shield, Damage, Misc) via a
  curated schema for the fields worth surfacing, with an "all fields" raw view
  behind it so nothing is unreachable,
- shows vanilla vs. current vs. edited for every field,
- writes **sparse** edits into `FighterMod.params`,
- shows one prominent notice, not a per-field badge, when the selected entry is
  a `SlotClone`: every value here is shared with the donor.

## How edits reach the three destinations

Every roster or trait edit must land in all three, and each has a different
failure mode:

**Project (in memory + `modproject.json`).** Sparse, keyed by stable
`RosterKey`/param path. This is the source of truth; the index and every view
are derived from it.

**Export (mod folder + Smashline source).** `export.rs` rebuilds
`ui_chara_db.prc` and each edited `fighter_param.prc` from current base state +
sparse overrides, and emits the scaffolded fighter/effect directories.

The export path has an existing hazard worth stating: verification here produces
a report, and the report must be surfaced, not reduced to a pass/fail bit. If a
param edit targets a field that no longer exists in the base file, or a roster
override names an entry the current library no longer provides, the user must be
told which one — `R-66` and `R-31` own this.

**Live.** Two different mechanisms, and conflating them is the trap:

- *Traits* can go live. `fighter_param` values are read into the fighter's data
  at load; a live param poke means sending the changed field to `slight_replica`
  and having it write the runtime value. This is genuinely useful (tweak weight,
  see it immediately) and is `R-64`/`R-65`. Until `R-88`, the shipped live path
  is file-based next-load (`visionary_roster_live`), not a mid-match poke.
- *CSS layout and roster membership* today go live as: write the modified UI
  files into `visionary_roster_live`, then ask ARCropolis to reload them and
  return to the CSS. The plugin already resolves the ARCropolis API
  (`arcrop_load_file`, `arcrop_register_callback`) in `effect_viewer/arcrop.rs`,
  which is the hook this uses. `R-32` must state the limitation in the UI rather
  than showing a live indicator that means nothing. **Phase 7** makes it truly
  live: patch the in-memory `ui_chara_db` and rebuild the grid without leaving
  the menu (`R-80`…`R-87`), and enforce the costume pin at selection time
  (`R-85`).

Today's live roster path (`R-32`, `src/roster/live.rs:60` `apply_live` → file
write + `resource_reload.rs:559` `replace_loaded_file` best-effort) is kept as
the fallback. The new in-memory path (`R-84`) sits beside it, behind a flag,
and both share the same `RosterMod` sparse input so export and live cannot
diverge.

New wire messages follow the existing framing exactly
(`<TCP_MESSAGE>{"header":...,"body":...}</TCP_MESSAGE>`) and the wire structs
must match the plugin side field-for-field, as `game_link.rs` already warns.

Editor -> plugin: pin UI character slots (Stage 1 shipped)
- Command: `pin_ui_chara`
- Payload: an array of objects: `[ { "ui_chara_id": <u64>, "slot": <u8> }, ... ]`
- Semantics: the plugin stores the mapping (`roster_pin.rs:29` `set_pins`) and —
  once `R-85` lands — will enforce the pinned costume when that `ui_chara_id` is
  selected in the game's UI. Stage 1 is implemented (`game_link.rs:1361`
  ↔ `debuggable_server/mod.rs:363` ↔ `roster_pin.rs:29`); Stage 2 (runtime
  enforcement) is `R-85` and needs the hook from `R-80`.

Editor -> plugin: live CSS order (Phase 7, `R-84`)
- Command: `live_css_order`
- Payload: `{ "order": { "<name_id>": <i8 disp_order> }, "hidden": ["<key>", …] }`
- Semantics: patch the resident `ui_chara_db` at the offsets from `R-81`, then
  invoke the rebuild trigger from `R-83`. Kept in sync with the sparse project
  write so a later export reproduces the same roster without a second edit.

### Phase 7 — Fully live CSS (why it needs decompilation)

File-based live already works for export and for "back out to the CSS" reload.
What it cannot do — and what makes a live editor feel live — is:

1. Reorder or hide a CSS cell **while the menu is open** and see the grid move
   immediately (requires `R-80` loader + `R-81` offsets + `R-83` rebuild hook +
   `R-84` patcher).
2. Make a slot-backed clone own its donor's cell so picking that cell launches
   the authored slot without the player manually picking the costume (requires
   `R-85` selection hook; file edits alone always leave it as "donor, whatever
   costume the player picks", as measured in the `cNN_index` audit above).
3. (Separate track) Change a fighter-wide value like `weight` and feel it
   **mid-match** without reloading the fighter (`R-88`).

All three are in-memory patches and therefore need measured addresses, not
guesses. The `research/decomp/ssbu-re/` harness, the Ghidra projects
(`ssbu_main` / `ssbu_1304`), and the existing probe tooling
(`resource_reload.rs:619` `resident_probe`, `debuggable_server/mod.rs:308`
`peek`, `register_ui_db_probe` at `resource_reload.rs:670`) are the workbench.
The task board's Phase 7 spells out the exact order so a wrong offset is caught
by a test rather than by a corrupted menu.

## Maintainability constraints

These are the rules that keep this from becoming another `app.rs`:

1. **No roster logic in `src/app.rs`.** It gets the tab dispatch and one state
   field. `R-70` checks this.
2. **The index is derived; the project is authored.** No UI mutates the index.
3. **Sparse edits only.** No whole-file copies in the project, for params or UI
   files. Whole-file copies cannot merge and cannot survive a library change.
4. **Every guessed game-format fact is a research task with a written answer**,
   not an inline assumption. `R-20`, `R-21`, `R-40` exist for this reason and
   block the work that depends on them.
5. **Backing-agnostic.** Adding `RosterBacking::NewFighterId` later must not
   require edits to `css.rs`, `traits.rs`, or their views.
6. **Tests only where a wrong answer is silent.** Param path resolution, sparse
   merge under mod-load-order conflicts, and project v2→v3 migration are silent
   when wrong and get tests. UI arrangement and file scaffolding are visible on
   first run and do not.
