# Visionary TODO

This file contains the active backlog only. Completed work is kept in Git history rather than
repeated here. Each open entry is a complete work order: it names the user-visible result, the
surfaces it must reach, the project-import behavior it must preserve, and the evidence needed to
close it.

## How to work this list

1. Take the first unblocked entry marked `[ ]`.
2. Change it to `[~]` with the date before editing; change it to `[x]` only when its definition
   of done is satisfied.
3. Keep the repository on the consolidated `main` branch. Do not create task branches.
4. If the evidence changes the shape of a task, rewrite the task and leave it open rather than
   silently dropping it.

Status key: `[ ]` open · `[~]` in progress · `[x]` done · `[!]` blocked, with the reason inline.

## Cross-surface contract

An edit that reaches one surface and not the others is a bug, not a partial feature. A value the
user can change in a panel but that never reaches the game is misleading; a value that reaches
the game but is absent from export is a mod that does not ship what was previewed.

Every ACMD capability lives on these five surfaces. A task is not complete until it is coherent
across all five, or explicitly says which surface is out of scope and why.

| # | Surface | Where | Requirement |
|---|---|---|---|
| 1 | **Parse + IR** | [acmd.rs](src/acmd.rs), [data.rs](src/data.rs) | The call becomes a typed variant instead of falling into `Raw`; unknown tails remain verbatim so export can reproduce the author's macro. |
| 2 | **Panel** | [app.rs](src/app.rs) | The user can see and change it, with a timeline placement when it has a frame or range. |
| 3 | **Live** | [game_link.rs](src/game_link.rs) and the relevant plugin hook | The plugin captures the primitive and applies safe suppress, override, or inject rules. |
| 4 | **Export** | `emit_*` in [acmd.rs](src/acmd.rs) and [mod_export.rs](src/mod_export.rs) | Regenerated source reproduces additions, deletions, retimes, values, and the original command family. |
| 5 | **Write-back** | [acmd_src.rs](src/acmd_src.rs) | Value edits rewrite only the user's existing argument spans; structural edits are reported, never guessed. |

Two source-output paths have different rules:

- **Generated export** may regenerate a complete function from the IR. Additions, deletions, and
  retimes are allowed when the generated source is valid.
- **Write-back to a linked project** preserves the user's macros, comments, formatting, and file
  layout. It rewrites existing values and only moves a source block when the structure is proven
  safe. Anything structural or ambiguous goes in the report with the macro and reason named.

### Project import is one live-apply transaction

`modproject.json` is the complete editable project, not a snapshot of the currently selected
move. Loading it must:

- replace the previous project, then materialize every saved fighter, move, ACMD category, effect
  call, sound script, expression script, live tweak, effect alias, and authored EFF operation;
- stage all data in the editor's keyed stores before publishing live rules;
- suppress intermediate per-move and per-field sends while the project is being rebuilt;
- publish the final flattened union for each live rule family only after every saved move has been
  materialized, and restore carrier/alias/tweak state as part of that same import transaction;
- never apply only the selected move, stream stale intermediate states, or require the user to
  reopen moves before their saved edits reach the game.

“All edits at once” means one coalesced project restore from the user's perspective. The wire may
use one full-list message per protocol family, but it must not expose the partially rebuilt
project between those sends. New editable data must be included in project serialization,
`build_project`, `load_project_from`, and the aggregate live-rebuild path.

## Definition of done

- [ ] The five surfaces are coherent, or the task explicitly names its out-of-scope surfaces.
- [ ] The edited data round-trips through `modproject.json` and project import restores every
      saved move/category, not only the selected move.
- [ ] Project import publishes a complete live union after materialization; tests cover the
      absence of intermediate sends and the presence of the final replacement.
- [ ] A real source call has a parse → emit → parse round trip with identical typed data. Unknown
      lines and tails remain lossless.
- [ ] A source-sync test proves that a value edit changes only its intended argument span, while
      a structural or ambiguous edit is reported with a reason naming the macro.
- [ ] `git diff --check` passes.
- [ ] Focused locked tests pass (`cargo test --locked --bin visionary <filter>`); run the full
      desktop suite when shared parsing, project persistence, or live-rule code changes.
- [ ] `bash build_check.sh` passes for changes that affect the desktop build.
- [ ] `bash plugins/slight_replica/scripts/build.sh` passes when the plugin is touched. A build is
      not game/runtime proof; report emulator, hardware, and UI evidence separately.
- [ ] [README.md](README.md) is updated when the user-visible behavior changes.

## Guardrails

- A macro name is not enough to establish its family. Check the linked signature and measure real
  source arities before adding slots or a typed variant.
- Argument slots belong to one family. Do not reuse a table across `ATTACK`, `CATCH`, `AREA_WIND`,
  sound, expression, or status calls.
- Source and live surfaces may need different discriminators: source can use token shape, while
  the wire may need argument count or a captured numeric identity.
- Hash40 values may arrive on the wire as integer-tagged Lua values. Accept the runtime form that
  actually arrives and preserve the authored source spelling separately.
- Do not put SD-card I/O on a per-frame plugin path. Use the throttled poller.
- Optional wire fields must remain backward-compatible with older plugin builds.
- Do not claim native Windows, emulator, physical Switch, or runtime success from a host build,
  cross-build, archive, or offline test alone.

## Active backlog

The entries below are the remaining inherited work plus the currently open upstream requests.
The GitHub issue number is part of the task identity; do not infer a request from an old local
label or a completed entry that used to have a similar name.

### [ ] #26 — Make sound calls structurally editable

[Upstream issue #26](https://github.com/Common-Leap/Visionary/issues/26) says sounds can be seen but
their frame cannot be changed and calls cannot be added or removed. The existing sound work covers
the measured name-edit path; this is the remaining scheduling and list-editing request.

Acceptance:

- The sound timeline and **Sound & feedback** panel can retime an existing sound and add or remove
  a supported sound call without losing `STOP_SE`, suppression tails, loop sites, or unknown source
  lines.
- The full `sound_` statement tree, not only changed rows, is stored in the project and emitted so
  an installed category does not silence calls the editor did not display.
- The live surface either suppresses/injects the exact measured call safely or clearly reports a
  source/export-only structural change. No rule may be keyed from the edited sound instead of the
  pristine call the game executes.
- Generated export and linked-source write-back have separate behavior: generated source may add,
  remove, or move calls; linked source rewrites existing values and reports structural operations
  unless a safe source insertion/removal contract is proven.
- Project import restores sound edits for every saved move and publishes them with the complete
  sound/live-rule union only after all project data is staged.

### [ ] #25 — Make expression scripts structurally editable

[Upstream issue #25](https://github.com/Common-Leap/Visionary/issues/25) says expression calls are
visible but their frame cannot be changed and calls cannot be added or removed. Existing measured
camera, rumble, and partial-rate value controls do not close this request.

Acceptance:

- The expression timeline and panel can retime supported calls and add/remove supported expression
  calls with the correct macro-specific arguments.
- Unknown expression statements remain source-preserved and visible; a structural operation that
  cannot be represented safely is refused with a useful source-only explanation.
- Parse/IR, generated export, live capture/rule/injection, and source write-back all agree on site
  identity and argument shape. Live edits use a measured native identity rather than an array
  index that can change after a loop or branch.
- The complete edited `expression_` script is serialized and restored for every move. Project
  import publishes the final expression rules together with the other categories, never one
  partially loaded move at a time.

### [ ] #24 — Expose the `EFFECT_FOLLOW_FLIP` tail value

[Upstream issue #24](https://github.com/Common-Leap/Visionary/issues/24) reports that the last
`effect_follow_flip` value, described by the reporter as its rotation control, cannot be edited.
This is separate from the completed X/Y/Z transform-label correction: the missing control is in
the macro-specific tail after the shared transform arguments.

First verify the actual macro signature and the meaning of the final tail token (for example, a
flip-axis/rotation selector) before naming the widget. Preserve the authored token and do not
turn a symbolic `EF_FLIP_*` value into an invented numeric enum.

Acceptance:

- The parser/IR retains the exact tail with a typed, macro-specific field only where its arity and
  meaning are measured; unsupported tails remain verbatim.
- The panel labels the field honestly and lets the user edit the supported value without hiding
  the alternate graphic or shared X/Y/Z transform controls.
- Export reproduces the selected flip family and its complete tail. The plugin hook/capture path
  applies a changed value only when the native live identity is proven; otherwise the UI reports
  source/export-only rather than claiming live success.
- Linked-source sync rewrites only the existing tail argument when safe and reports a command or
  tail-shape change as structural. The complete effect edit is persisted and restored by project
  import with the final flattened effect rule union.

### [ ] #23 — Highlight the selected hitbox

[Upstream issue #23](https://github.com/Common-Leap/Visionary/issues/23) asks for a visible
difference between a selected hitbox and the other hitboxes, similar to the existing effect
selection feedback.

This is presentation state, not a new ACMD capability. Its explicit surface scope is **Panel**
and renderer/timeline presentation only; Parse + IR, Live, Export, and Write-back are out of scope
because selecting an item must not create or change a game edit or project record.

Acceptance:

- Selecting a hitbox in the list, timeline, or viewport produces the same obvious highlight and
  keeps the existing attack/grab/wind category colors readable.
- The highlight follows selection changes, deletion/reindexing, move changes, and timeline
  seeking without sticking to the wrong hitbox.
- The selected state does not alter hitbox values, live rules, generated source, or project import.
- A focused UI/model test covers selection identity and the visual-state decision for at least one
  ordinary attack and one non-attack collision family.

### [ ] E1 — Finish measured movement, kinetics, and status coverage

The typed movement slices are in place, but the parent remains open for the broader status-module
and kinetic families that are still source-preserved. Do not turn a textual occurrence into an
editable feature without a buildable signature and a safe live identity.

Remaining scope to measure and decide:

- the `sv_kinetic_energy` families and other status-module mutations not covered by the existing
  exact receiver/arity slices;
- the read-only `KineticModule::get_sum_speed_y` and getter-driven status expressions, where the
  first question is whether there is an honest editable concept at all;
- any additional direct kinetic or WorkModule operation found in a current corpus audit.

For every family that is actually typed, carry the exact measured form through Parse + IR, the
Movement/Status panel and timeline, live capture/rule/hook, generated export, and value-only
source write-back. Keep unmeasured receivers, malformed arities, symbolic-only live identities,
branches, and loops raw or explicitly source-only. Movement preview is out of scope unless a
separate task defines and verifies the animation/kinetic simulation contract.

Every accepted movement edit must be represented in the project file and restored for every saved
move by the one-transaction project import path. The final live union, not the currently selected
move, is what reaches the game.

#### [!] E1r — `MotionModule::set_frame_partial` binding mismatch

Blocked pending version-matched source or runtime evidence for the missing boolean argument. The
retained source calls use three arguments after the receiver, while the pinned native binding has
four (`receiver, part_kind, frame, sync`). Native wrapper disassembly confirms ABI normalization
but does not identify the Lua/source default. Do not guess it.

Until that evidence exists, preserve the three-argument calls verbatim, expose them as read-only
source-only rows when useful, and do not add a parser variant, export helper, write-back slot, or
live hook that invents the boolean.

## GitHub issue reconciliation

Checked against the upstream repository on 2026-08-15, excluding #5 and #4 as requested.

- Open #23, #24, #25, and #26 were not equivalent to an active backlog entry, so they are
  included above as separate tasks. #24, #25, and #26 extend already completed baseline features
  but ask for distinct controls or structural edits.
- #27 is complete: the linked-source implementation now indexes the whole project, preserves
  multi-file ownership, reports duplicate identities, and keeps per-file write-back safe. It is
  omitted from the active list with the other completed work.
- Closed issues #1–#3 and #6–#22 are not re-added: their completed goals were removed from this
  active list. #4 and #5 are intentionally excluded even though #5 remains open upstream.
