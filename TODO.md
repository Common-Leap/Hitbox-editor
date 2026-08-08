# Visionary TO DO

A standing backlog of ACMD coverage gaps and known rough edges. Each entry is a complete
work order: any model can cold-start into this file, take the top unblocked task, and finish
it without prior conversation.

## How to work this list

1. Read **Working agreement** and **Definition of done** below. They apply to every task.
2. Take the first task whose status is `[ ]` and whose **Blocked by** is satisfied.
3. Set it to `[~]` with your date before starting, `[x]` when it meets the definition of done.
4. Commit the code and the status flip together on the consolidated `main` branch. Do not
   create task branches; this repository intentionally keeps one working branch.
5. If a task turns out to be wrong or impossible as written, do not silently drop it —
   rewrite the entry with what you learned and leave it `[ ]`.

Status key: `[ ]` open · `[~]` in progress · `[x]` done · `[!]` blocked (say why inline).

## Working agreement

**The rule this whole file exists to enforce: an edit that reaches one surface and not the
others is a bug, not a partial feature.** A value the user can drag in a panel but that never
reaches the game is a lie; a value that reaches the game but not the export is a mod that
doesn't ship what was previewed.

Every ACMD capability lives on **five surfaces**. A task is not done until it is coherent
across all five, or until the entry says explicitly which surface is out of scope and why.

| # | Surface | Where | What it means |
|---|---|---|---|
| 1 | **Parse + IR** | [acmd.rs](src/acmd.rs), [data.rs](src/data.rs) | The call becomes a typed variant instead of falling into `Raw`. Unknown tails are kept verbatim so an export can reproduce the caller's own macro. |
| 2 | **Panel** | [app.rs](src/app.rs) | The user can see and change it. Timeline placement if it has a frame range. |
| 3 | **Live** | [game_link.rs](src/game_link.rs) + [hitbox_viewer/mod.rs](plugins/slight_replica/src/slight/hitbox_viewer/mod.rs) / [acmd_hooks.rs](plugins/slight_replica/src/slight/effect_viewer/acmd_hooks.rs) | The plugin hooks the primitive, captures it, and applies suppress/override/inject rules for it. |
| 4 | **Export** | `emit_*` in [acmd.rs](src/acmd.rs) | Regenerating the function reproduces the call, including anything added, deleted, or retimed. |
| 5 | **Write-back** | [acmd_src.rs](src/acmd_src.rs) | Editing a value rewrites that argument in the user's own source, touching nothing else. Structural change is *reported*, never guessed. |

Two export paths, different rules — do not conflate them:

- **Export to `acmd_source/`** regenerates the whole function from the IR. Structure is free:
  add, delete, retime.
- **Write-back into the user's linked project** rewrites argument *values* only. Their macros,
  comments, and formatting survive verbatim. Anything structural goes in the skip report with
  a reason naming the macro. Never write a guess.

### Traps that have already cost real time

- **On the live wire a hash and an int are the same thing, and matching on the type tag makes an
  edit silently dead.** `PLAY_SE`'s signature says `Hash40`; the lua stack passes it as
  `L2CValueType::Int`. Kirby's up tilt hands over `0x10556b83cc`, which is exactly
  `hash40("se_kirby_swing_l")` — the right value under the wrong tag. The plugin's sound rule
  matcher required `LuaArg::Hash` and therefore matched **no sound in the game**, while the
  editor's `as_hash` already accepted `Int` so live *capture* worked perfectly. **Two readers of
  one stream with different tolerance is the worst shape this can take**: the feature half-worked,
  which reads as "nearly right" rather than "wrong in one place", and four game restarts went into
  diagnostics before the branch that bailed was made to say so. This is
  `SEARCH`/`CATCH`'s "the same fact needs a different test on each surface" one step over —
  a source parser can tell `Hash40::new("…")` from an integer literal and a wire reader cannot.
  **Accept both, mask to 40 bits, and write back under the tag that arrived.**
- **A guard that enumerates "everything that exists" is correct when written and drops the next
  family silently.** Four instances in two days, none of which failed loudly and none of which any
  test caught: the move list filtered to six substrings and hid 65% of the corpus's sound scripts
  (R5); `load_from_captures` returned early on hitboxes and effects being empty and discarded
  captured hurtboxes and sounds (R7); `select_move` cleared four per-move fields and left the
  sound ones populated (R8); and `rebuild_script_from_hitboxes` retained only `Raw` and deleted
  every `HIT_NODE` the moment B4 typed them. In every case the code was *right* when written and
  nothing re-examined it. The compiler cannot help — these are `is_empty()` chains and
  `starts_with` lists, not matches. **Two things that do: make the decision a free function over
  its inputs so a test can drive it** (inline in a `&mut self` method it is unreachable, which is
  why all four survived full suites), **and assert the user-facing message names every family**,
  which turns an invisible drop into a wrong string. Be honest that the second is a reminder and
  not a proof: a fifth family only fails the test if someone adds its name to the expected list.
- **Argument slots are per family.** An id that means one thing in `ATTACK` means something
  else in `CATCH` or `AREA_WIND`. Never reuse a slot table across families — a cross-family
  write silently corrupts a different call. Give each new family its own table.
- **A new family whose name extends an existing one gets swallowed by prefix bucketing.**
  Several dispatch points bucket on `func.starts_with("ATTACK")` rather than on the exact name.
  B1 found two: the editor's `hitboxes_from_captures` read a captured `ATTACK_ABS` through
  `ATTACK`'s 36-slot table (sixteen arguments against thirty-six — it came back as a plausible,
  wrong hitbox), and the plugin's `is_collision_func` armed the clear-all gate for a call
  nothing clears. The *parser* was safe only because it matches `macros::NAME(` with the paren.
  Before adding such a family, grep for `starts_with(` on the prefix you are extending and fix
  every hit; then add the exact-name arm **above** the prefix one.
- **A walk that unrolls loops will count a line once per iteration.** `eval_effect_stmts` and
  `eval_stmts` both run a `for` body `count` times, which is right for the calls they produce
  and wrong for anything that names a *line of source*: a report, a site ordinal, a residue
  buffer. C6 read `COL_NORMAL` as 16 losses against a true 8 this way, and B4 hit the same
  thing with hurtbox site ordinals. The fix both times was to rewind the per-line state at the
  top of each iteration and step it once for the whole body — `call_macro_ordinals` and
  `EffectStmt::Loop` in `eval_effect_stmts` are the two worked examples. **The tell is a count
  that is an exact multiple of the truth**, so check a new number against `main` in a worktree
  before believing it; a plausible number is the easiest kind to ship.
- **A macro's name is not its family. Check `lua_const` before modelling it.** `COL_PRI` and
  `COL_NORMAL` read as body collision for four tasks running, and B4 modelled them as hurtbox
  statements on the strength of it. They are `MA_MSC_CMD_COLOR_BLEND_COL_PRI` and
  `MA_MSC_CMD_COLOR_BLEND_COL_NORMAL` — the `FLASH` family. Grepping `lua_const.rs` for the
  macro's name takes a second and gives you the `MA_MSC_CMD_<FAMILY>_` prefix the game itself
  files it under; that prefix is the answer to "which existing table does this belong in", and
  it is usually a table you already have. The cost of guessing is not just a wrong doc comment:
  it puts the control in the wrong panel and sends the fix down a new-type path when a one-row
  path existed. **When `lua_const` is silent, the signature is the next-best oracle** — it has
  no constant for any of B3's five macros, and it was `WHOLE_HIT`'s `(hit_status)` argument that
  showed it to be B4's family rather than the hitbox tuner its name and its entry both claimed.
- **Not every macro in an entry's list belongs to that entry's shape — check the signature of
  each, not just of the first.** B3 was framed as "modifiers attached to the hitbox id they
  target" and named four macros. Two of them (`ATK_HIT_ABS`, `ATK_LERP_RATIO`) take no id at
  all, so the framing could not describe them; they were not a smaller version of the task but a
  different one, and saying so in the entry was the deliverable. A grouping is a hypothesis until
  each member's `macros.rs` line is read. The tell that you are about to do this wrong is
  planning the work for a *list* rather than for a shape.
- **A shared numbering space is a coupling, and site ordinals are the place it bites.** Families
  that are edited by ordinal-into-source need their own counter each. Folding B3's modifiers into
  `HurtSite` would have meant adding an `ATK_POWER` to a move silently shifted every later
  `HIT_NODE`'s site, retuning a call the user never clicked — and no existing test would have
  failed, because each family passes its own tests in isolation. Test the boundary from *both*
  sides when adding a family beside an existing one.
- **The same trap again, and worse, across the wire: `Hitbox.category` is not the plugin's
  category.** They agree for attack (0), grab (1) and wind (2) and then diverge — the editor's
  `CAT_ABS` is 3, the plugin's is 4, because the plugin's space also carries hurtbox state,
  which took 3. B5 found that every live `ATTACK_ABS` edit since B1 had gone out as category 3
  and been read as a hurtbox rule: **silently dead, for two whole tasks.** Everything now goes
  through `game_link::wire_category()`. The general rule: *two id spaces that agree on their
  first few members are not the same space, and the agreement is exactly what hides it.* When
  adding a family, add its wire value to that function and to the test that pins the mapping.
- **The same fact needs a different test on each surface.** `SEARCH` and `CATCH` are each
  written with and without their capsule arguments, and every surface has to tell which. In
  source text the discriminator is the *shape of the token* — a coordinate versus a `*CONST`.
  On the live wire that test is wrong, because an int and a float are both just numbers there,
  and it happily read a 14-argument call's collision kind, hit status and undocumented int as a
  capsule of `[2.0, 1.0, 60.0]`. There the discriminator is the argument *count*. Do not carry
  a guard across surfaces without asking what signal that surface actually has.
- **A round-trip oracle cannot see a value lost on the way in.** `check_hitbox_fidelity`
  compares the export against the *parsed model*. Anything the parser drops or defaults agrees
  with itself on the way out, so the check is green — which is how four short-form `CATCH`
  calls sat in the corpus for the whole project reading as ordinary grabs instead of Kirby's
  swallow. When a parse change is the thing at issue, **assert against the original text.**
- **A parser that "preserves unknown lines verbatim" still has to preserve their braces.**
  `parse_stmts` skipped every lone `}` and kept an `if …{` as a one-line `Raw`, so a runtime
  branch was flattened: the closing brace vanished and both arms of an `if`/`else` were promoted
  to unconditional. **35 of 236 `game_` scripts in the corpus exported a function with more `{`
  than `}`** — a mod that does not build — and the round-trip oracles all passed, because they
  re-parse the export with the same lenient parser that wrote it, and an unbalanced function
  reads back exactly as it went in. The general rule: *a "verbatim" escape hatch that is a
  single line cannot represent a construct that spans lines.* `AcmdStmt::RawBlock` is the shape
  that can. When adding one, check what a `}` on its own line currently does.
- **Verbatim round-trip is the wrong bar when the source itself is malformed.** Six corpus
  `sound_` scripts are mis-indented — the dumper does not re-indent after an `else` — so a
  byte-equality gate fails on a *dumper* bug and pressures you into reproducing it. Assert the
  three properties that actually matter instead: the trimmed lines are the same (nothing lost),
  emitting twice is a fixed point (a rewritten file does not drift), and the byte-exact count is
  pinned (formatting cannot rot unnoticed). See
  `every_sound_script_in_the_corpus_survives_a_round_trip`.
- **A bullet written from memory is not a measurement, even when you wrote it.** C6b's
  `CANCEL_FILL_SCREEN` item claimed C3 modelled `FILL_SCREEN_MODEL_COLOR` and that the reset was
  a `COLOR_COMMANDS`-shaped row. C3 does not, and it is not — C6's own result table three
  hundred lines up had already recorded "a different family", and the real arities are 12 and 2.
  Both errors were caught by the arity check the bullet itself told the next reader to run, so
  **run the check on the entry you are about to implement before trusting its framing**,
  especially when the entry cites a *conclusion* about another entry rather than a measurement.
- **Two builders that accept the same macro name export it twice.** A `CaptureLine` records no
  source function, so every capture builder filters the one stream by name. While the name
  tables are disjoint this is invisible; the moment a macro joins a second table, one live call
  is written into two different exported functions. Adding a name to a table is therefore a
  question about *every other* table too — grep the name across the builders before adding it,
  and if two must claim it, make one of them claim it only in the case it truly needs.
- **Two sources of truth for arity, and they disagree. You need both.**
  `/home/leap/.cargo/git/checkouts/smash-script-*/*/src/macros.rs` tells you what *compiles*,
  which is what an export must emit. The vanilla archive tells you what you must *parse* — and
  it omits optional arguments that smash-script declares. A2 found `ATTACK_IGNORE_THROW`
  written with 33 arguments against a 36-parameter signature. Count the corpus per family
  before writing a slot table:

  ```bash
  # counts | arity, excluding the leading `agent` — matches the tables in this file
  grep -rho 'macros::YOUR_MACRO([^;]*' ~/.cache/visionary/script-cache/ | \
    awk -F',' '{print NF-1}' | sort | uniq -c
  ```

  If a family has more than one arity, the discriminator must be the argument *shape*, not the
  count — a length test breaks the moment a call is short for a different reason. When every
  argument of a family is a bare number there *is* no shape, and then the command name is the
  layout: refuse a call whose length disagrees with its name rather than reinterpreting it.
- **The plugin can hook a primitive the export cannot write.** `sv_animcmd` has every
  primitive; `smash_script::macros` wraps only some. A1 found `macros::AREA_WIND_2ND` missing
  while the plugin hooked `sv_animcmd::AREA_WIND_2ND` happily — so a capture came back live,
  drew correctly, and exported a project that did not build. Before adding a family, grep
  `macros.rs` for **every** member by name, and make the verifier block the ones that are not
  there. Do not assume a hooked primitive is an emittable one.

  **Grep the whole prefix, not each name.** Declarations are not spelled consistently — some
  are `fn NAME<A: ToF32>`, others `fn NAME <A: ToF32>` with a space — so a per-name
  `fn NAME[<(]` pattern reports a wrapper missing when it exists. That false negative would
  block an exportable macro, which is the same class of bug in the other direction. List them
  all and compare against the list:

  ```bash
  grep -rhoE 'pub unsafe fn [A-Z_0-9]+' ~/.cargo/git/checkouts/smash-script-*/*/src/macros.rs \
    | sed 's/pub unsafe fn //' | sort -u
  ```

  Confirmed missing so far: `AREA_WIND_2ND` (A1), `LAST_EFFECT_SET_WORK_INT` (found while
  auditing C1; C7 now emits a local helper over the linked primitive), `FLASH_SET_DIRECTION`
  (C3 — 8 corpus uses, so corpus frequency is no guide to whether a wrapper exists; check every
  member every time).
- **"Parsed into the IR" is not the same as "reaches the export."** The effect export is
  generated from `EffectCall`s, not from `EffectScript` statements, so a macro can have its own
  `EffectMacro` variant, parse perfectly, and still be dropped by `eval_effect_stmts` on the way
  to `EffectCall` — after which the export never sees it. A3 found `LAST_EFFECT_SET_RATE` in
  exactly that state, described in this file as already done. Before believing a surface is
  covered, follow the value all the way to the emitted text; and note that
  `cached_scripts_round_trip_through_the_emitter` only compares `(spawn_func, effect_name)`
  pairs, so it will not catch a dropped field for you.

  The general form of this was **C6**: a line with no typed variant is not merely unmodelled,
  it is *deleted* by the export. C3 found 69 more of them after A3 found 27, C5 measured the
  rest, and C6 fixed most of it — the export now copies unmodelled lines through in position,
  and 19 of 132 effect scripts still lose one rather than 28. You no longer have to assume:
  open the move in the editor and read the generated-source pane, which names both what the
  export will not write and what it copies through without understanding. C6b lists what is
  still going.

  **The root cause is worth remembering, because the other three categories do not have it.**
  A `game_` script is emitted from its statement tree, so anything in the tree survives whether
  or not the emitter understands it. The effect export is regenerated from a flat call list, so
  only what became a typed call could come out. When a new category is added (`sound_`,
  `expression_` — D1 and D2), pick the tree, not the list, and this whole family of bugs never
  starts.

  **The sharp edge: modelling a macro can make its loss quieter instead of fixing it.** C1
  gave `LAST_EFFECT_SET_COLOR` a typed variant, and 32 corpus lines promptly *stopped* being
  reported as dropped — they no longer parsed as `Raw`, so the dropped-line check could not see
  them, and they were still discarded on the way to `EffectCall` because they bind to no spawn.
  Net effect of modelling: one line recovered, thirty-two silenced. The guard is
  `EffectScript::to_effect_calls_reporting_losses`, which returns whatever the walk threw away;
  **anything that resolves a macro into a call must report what it could not resolve there**, or
  it repeats this. A test asserting the *premise* of a loss — C5's
  `a_line_the_export_cannot_reproduce_is_named_rather_than_silently_deleted` fails loudly if its
  example line becomes exportable — is what caught it.
  **The other edge of the same knife: giving a macro a variant can delete it somewhere else.**
  Code that keeps "everything we do not handle" by testing for `ExcuteStmt::Raw` is really
  testing "everything without a variant", and those stop being the same set the moment you add
  one. B4 hit this in `rebuild_script_from_hitboxes`, where dragging a hitbox silently deleted
  every `HIT_NODE` in the move. **Before modelling any macro, grep for filters on `Raw` — and
  on variant lists generally — and make them exhaustive `match`es first**, so the compiler
  forces a decision instead of defaulting to a deletion. Both halves of this trap have now cost
  real time; assume the next family has a third.
- **A macro that names no target binds to the line above it, and nowhere else.**
  `LAST_EFFECT_SET_*` modifies whatever spawned last, so there is nothing in the call to match
  on. Bind it to the immediately preceding recognised spawn and refuse otherwise — reaching
  further back silently retunes an effect the user never touched. The rule must be implemented
  identically in the script parser, the live-capture reconstruction, and the write-back
  scanner; A3 has all three, and they are the pattern to copy.
- **Symbolic constants need both directions.** The editor holds names, the wire wants numbers.
  A new constant needs a `ConstTable` in [param_labels.rs](src/param_labels.rs) plus
  `decode_const`/`encode_const` coverage, or the live path sends `None` and the game keeps its
  own value while the panel claims otherwise.
- **New collision families must be added to the plugin's `is_collision_func`**
  ([hitbox_viewer/mod.rs:494](plugins/slight_replica/src/slight/hitbox_viewer/mod.rs:494)) or
  the capture stream drops them. It is documented as being kept in step with the editor's
  bucketing — keep it so.
- **No SD I/O on a per-frame path in the plugin.** Free on the dev machine's Linux, a whole
  frame budget on a Windows tester's. Throttled work goes through `slight::sd_poll`.
- **Old plugin builds must keep working.** Every wire field is `Option` +
  `skip_serializing_if`; a plugin that predates a field ignores it and applies the rest.
  Keep that property.

## Definition of done

A task is done when all of these hold. Copy this list into your working notes; don't
paraphrase it from memory.

- [ ] The five surfaces above are coherent, or the entry names the out-of-scope surface.
- [ ] `bash build_check.sh` passes.
- [ ] `cargo test` passes (**512 unit + 6 integration in the current desktop suite**; the integration ones
      are [tests/deploy_plugin.rs](tests/deploy_plugin.rs) and shell out to `python3`),
      including the eight corpus oracles — run
      them by name with `cargo test cached_script`, `cargo test still_loses`,
      `cargo test unbalanced`, `cargo test survives_a_round_trip`,
      `cargo test is_typed_rather_than` and `cargo test lands_on_a_call`:
      `acmd_verify::tests::every_cached_script_survives_its_own_export`,
      `acmd::tests::cached_scripts_round_trip_through_the_emitter`,
      `acmd::tests::the_effect_export_still_loses_no_more_of_the_corpus_than_it_did`,
      `acmd::tests::no_game_script_in_the_corpus_exports_an_unbalanced_function`,
      `acmd::tests::every_sound_script_in_the_corpus_survives_a_round_trip`,
      `acmd::tests::a_partial_project_override_keeps_every_category_it_does_not_define`,
      `acmd::tests::every_sound_call_in_the_corpus_is_typed_rather_than_left_raw` and
      `acmd::tests::every_corpus_sound_site_lands_on_a_call_of_its_own_macro`. They run
      the new code over every script the app has ever fetched (currently 462 files under
      `~/.cache/visionary/script-cache`, ~1000 functions). The third asserts a *number* — how
      many effect scripts still lose a line — so it fails on a regression rather than only on a
      crash, and the fifth pins the byte-exact count for the same reason.
      **All eight return early and pass vacuously if that cache directory is missing**, and each
      carries its own guard against a corpus too thin to mean anything: `checked > 100` on three
      of them, `branching >= 30` on the unbalanced-function one, which would otherwise stay
      green if branches simply stopped being recognised, `132 with effects / 78 with
      hitboxes` on the merge one, and `total > 500` calls on the sound-typing one. Confirm the
      directory exists before
      trusting any of them. `ls` shows only 3
      entries; the files are nested per fighter, so count with
      `find ~/.cache/visionary/script-cache -type f | wc -l`.
      **Neither oracle is a full-fidelity check, so do not read a green run as one.** The
      effect one compares only `(spawn_func, effect_name)` pairs — A3's dropped rate passed it
      for as long as the bug existed. The per-field comparison lives in
      `check_effect_fidelity` / `check_hitbox_fidelity`; a new field must be added *there* or
      nothing is checking it.
      **They are also blind to a macro you have not modelled yet**, which is the trap when
      *adding* a family rather than changing one. An unparsed line exports verbatim as `Raw`, so
      it round-trips perfectly; all three were green on `WHOLE_HIT` before and after it was
      modelled. A green oracle says "nothing broke", never "the new thing works".
      **The seventh is the answer to that**, and the shape to copy when adding the next family:
      it counts, per script, the calls the *source text* writes against the ones the parser
      typed, so a member that is not recognised fails instead of falling back to `Raw`. It found
      15 scripts on its first run whose sound sat outside an `is_excute` block and had been
      invisible since D1a.
      **The eighth is the shape to copy when adding an *edit*** rather than a family: it checks
      that each resolved site lands on a call of its own macro, and that every call in the text
      is reached by the walk. A mis-sited edit is invisible to every other check here, because it
      writes a script that parses, compiles and round-trips — it is simply the wrong line.
      **It also records a measured absence**: zero of the 301 corpus sound scripts contain a
      `for`, so it says nothing whatever about looped calls, and a green run there must not be
      read as covering the site cursor's rewind.
- [ ] **A second, separate corpus gate exists and is easy to miss.** Every test needing real
      binary effect data — the whole of `eff_export::tests`, plus C6d's
      `a_refused_export_does_not_write_the_eff_files_either` — skips unless `VISIONARY_EFF_ROOT`
      points at an extracted `effect/` tree (the directory *containing* `effect/fighter/…`, e.g.
      an ArcExplorer `export/` folder). It is unset by default, so a plain `cargo test` on a
      fresh checkout silently skips all of them and still says 382. Set it before claiming an EFF
      or export-ordering change was verified.
- [ ] A round-trip test for the new family: parse a real vanilla call → emit → parse again →
      identical IR. Put the real call in the test, not a synthetic one.
- [ ] A write-back test asserting that a value edit rewrites *only* that argument span, and
      that a structural edit lands in the skip report with a reason naming the macro.
- [ ] The plugin builds if it was touched: `bash plugins/slight_replica/scripts/build.sh`.
      **That is the whole of what can be verified without a Switch or a running emulator.** The
      script only copies the `.nro` to `target/output/`; it does not deploy, so the `diag.txt`
      build stamp cannot be checked from a build alone. If you have a running game, check it;
      if you do not, say so in the entry rather than implying the live surface was exercised.

      **You probably do have one, and several closed entries wrongly say otherwise.** Measured
      2026-08-06: `/usr/bin/eden` and `/usr/bin/Ryujinx` are installed, SSBU
      (`01006A800016E000`) is installed under both, each has a
      `…/Arcropolis/romfs/skyline/plugins/` holding this plugin, and the last session wrote
      `~/.local/share/eden/sdmc/slight/diag.txt` on 2026-08-05. D1c, D1d and B4 each say this
      machine has no emulator; that was never checked and is false. **Before writing "needs
      hardware" in an entry, run `ls ~/.local/share/eden/sdmc/slight/diag.txt` and read the
      `build=` line** — it names the build actually installed, which is the thing that decides
      whether a live claim can be tested at all. It was 66 commits stale when this was written.
      What genuinely is not available here: a **physical Switch** (R3 — Eden's JIT is the
      blocker, so an Eden boot cannot settle it), a **Windows host** (R2 half 1), and a
      **game dump** (R1 — no `exefs/main` on this machine).
- [ ] [README.md](README.md) updated if user-visible behaviour changed. House style: plain
      imperative button labels, no ellipsis.

---

# Part 1 — ACMD coverage gaps

Ordered so that earlier tasks unblock later ones. **Position within a section is not a
priority ranking** — it was meant to be, but measuring the corpus after A3 showed it is not:
C3 (69 occurrences) beat C1 (65) and was taken first, and B4 (45) beats B3 (23). Each entry
carries its own measured counts; go by those. Blocking relationships are the only thing the
order still encodes.

Counts are occurrences in the local 462-file corpus, which is what the app has fetched so far
— a proxy for how often real scripts use a macro, not a census of the game.

## Foundations

### [x] A1 — Wind write-back into the user's own source

Done 2026-08-04. All five surfaces; `cargo test` 260 green, both corpus oracles run for real
(461 files present). The plugin was not touched — the live surface already hooked and injected
all four commands by name.

Wind edits now reach a linked project. The entry's premise held: the four commands share slots
0..=7, so one table indexed by argument position covers all of them, with the *names* differing
past slot 7 (`width`/`height` vs `radius`/`lifetime`) so the skip report reads correctly. Sites
are matched on command name **and** id, so a rectangular value can never land in a radial call.

**Two things this turned up that the entry did not predict:**

1. **`macros::AREA_WIND_2ND` does not exist.** `sv_animcmd` has all four commands and the
   plugin hooks all four, but smash-script only wrapped three — there is no
   `AREA_WIND_2ND` in `macros.rs`. The emitter wrote `macros::AREA_WIND_2ND(agent, …)`
   regardless, and **Add Wind Box → Rectangle created exactly that**, so every exported mod
   containing an editor-added rectangular wind failed to build. Add now produces
   `AREA_WIND_2ND_arg10` (and `_RAD_arg9` for radial, for the lifetime), and the verifier
   blocks the wrapper-less command with a message naming the replacement. Parsing still
   accepts it — the archive is allowed to contain it, only the export is refused.
   *Possible follow-up:* auto-upgrade a parsed `AREA_WIND_2ND` to `_arg10` by appending the
   lifetime the export already computes, instead of blocking. Deliberately not done — it is a
   macro substitution, and the house rule is to refuse rather than guess.
2. **The Lifetime slider did nothing.** Export rewrites the lifetime argument out of the
   timeline range (`end - start + 1`), so whatever the slider set was overwritten on the way
   out. The slider now moves `active_end` with it — but only for the commands that *have* a
   lifetime slot. The shorter forms end at an `AreaModule::erase_wind` on another line, and
   for those the end frame is not the call's to write. Getting this wrong reported a retime on
   every single edit, because a command with no lifetime "ends" at `u32::MAX`.

- **Landed:** `WIND_COMMANDS` / `WIND_MACRO_COMMANDS` / `is_wind_command` in
  [data.rs](src/data.rs) (the arity table lived in two places and now lives in one),
  `WindboxData::end_frame` as the single home of the lifetime → end-frame derivation,
  `wind_box_edits` in [acmd_src.rs](src/acmd_src.rs), an `ArgValue::ToF32` that writes `20`
  rather than `20.0` so both export paths put the same text in the file, and the verifier
  blocker.

**For whoever takes B3 or C1:** `ArgValue::ToF32` is the right variant for any slot generic
over `ToF32`, which is most of the non-`ATTACK` families. `ArgValue::Float` always writes a
decimal point, which is correct for `ATTACK`'s typed `f32` slots and wrong everywhere else.

### [x] A2 — `ATTACK_IGNORE_THROW` as a first-class hitbox

Done 2026-08-03. All five surfaces; `cargo test` 254 green, plugin builds.

**It was not the cheap task this entry predicted, and the entry's premise was wrong.** Kept
here in full because the reason is a fact about the corpus that the next family task needs.

The plan said: confirm the arity, and if it matches `ATTACK`, just carry the name. smash-script
declares both with the same 36 parameters, so that check passes. **The archive does not agree
with smash-script.** Measured across the local corpus:

| macro | arguments | occurrences |
|---|---|---|
| `ATTACK` | 36 | 386 |
| `ATTACK_IGNORE_THROW` | **33** | 1 |
| `ATTACK_ABS` | 16 | 32 |
| `CATCH` | 8 or 11 | 6 + 6 |

The archive writes `ATTACK_IGNORE_THROW` with the `x2`/`y2`/`z2` capsule options simply left
out. Read against `ATTACK`'s table it parses, and parses *wrong*: hitlag lands in the capsule,
`*ATTACK_LR_CHECK_POS` in hitlag, every property after that off by three. It would have
silently corrupted kirby/ThrowHi — the one real usage — and looked fine doing it.

So the capsule triple is now detected from the **arguments** (all three spelled `None`/`Some(..)`),
not from the macro name or the argument count, and everything past the transform shifts when it
is absent. Both the parser (`capsule_slots_present`) and the write-back slot table
(`shift_past_absent_capsule`) do this; they are separate code paths and both were wrong.

Exports always write the long form, since that is the signature smash-script declares and
therefore the only one that builds.

- **Landed:** `func` on `AttackCall`/`Hitbox` (serde-defaulted, so old projects load), family
  match in `parse_excute_block`, name in `emit_attack`, name-scoped site matching in
  `rewrite_hitboxes`, `func` carried off the live capture, `inject_command` on the wire, plugin
  dispatch in the inject arm, and a **Macro** dropdown in the hitbox properties.
- **Left alone deliberately:** the live *override* path needed no change — the plugin hooks
  both macros through the same `attack_hook!` and rules are keyed by category, id, and frame.
  Only *injection* (added or retimed hitboxes) had to learn the name.

**For whoever takes B1/B2:** do not trust a signature in `macros.rs` to tell you what the
archive wrote. Count the arguments in the corpus first — the one-liner that produced the table
above is worth rerunning per family.

### [x] A3 — `LAST_EFFECT_SET_RATE` as an editable field

Done 2026-08-04. All five surfaces; `cargo test` 268 green, both corpus oracles run for real
(461 files present). The plugin was touched and builds; the `diag.txt` stamp could not be
checked, because `scripts/build.sh` only writes to `target/output/` and deploying needs a
running game.

**The entry's premise was wrong, and wrong in the expensive direction.** It said the value
"round-trips untouched" with surfaces 1 and 4 already done. Surface 1 was half done and
surface 4 was not done at all: the rate was parsed into `EffectMacro::LastEffectSetRate` and
then **dropped** by `eval_effect_stmts` on the way to `EffectCall`. The effect export is
generated from `EffectCall`s, so it wrote no rate line — every vanilla effect that plays fast
or slow shipped at normal speed, silently. There are 27 such calls in the local corpus.

The one `LAST_EFFECT_SET_RATE` the emitter *did* write came from `LiveTweak::speed`, an
unrelated kind-global live control, which is what made the entry look already-done.

**The positional trap, settled.** The rate binds to the spawn *directly above it in the same
block*, and to nothing otherwise — not to "the last spawn seen". Anything in between breaks
the pairing: an `EFFECT_OFF_KIND`, a trail, or any line the parser keeps as `Raw`, because a
`Raw` line could itself be a spawn and then the rate belongs to it. This is stricter than the
game, whose "last effect" persists across frame blocks. It costs nothing: all 27 corpus calls
sit directly beneath a recognised spawn. Reordering and disabling then need no special case at
all — the rate is a field *on* the call, so it moves and disappears with it.

The same rule is implemented three times and the three must agree, or a value is read off one
call and written into another: `eval_effect_stmts` (script), `effect_calls_from_captures`
(live capture), and `spawn_and_modifier_sites` (write-back, renamed from
`spawn_and_rate_sites` by C1 when tint and opacity joined the rate).

**Also fixed on the way:**

- **The panel had no rate control at all**, so the checkbox/value pair is new. Off means no
  line is written, which is not the same as writing 1.0 — that distinction is the whole reason
  `rate` is an `Option` rather than defaulting to 1.0.
- **The live surface had no rate anything.** The plugin now hooks
  `sv_animcmd::LAST_EFFECT_SET_RATE`: it records the line so a captured move comes back with
  its rates, and rewrites the argument when a spawn rule retuned that spawn. Rewriting is
  required rather than calling `set_rate` — the script's own line runs last and would
  overwrite anything applied before it. `SpawnRule.rate` is `Option` + `skip_serializing_if`,
  so older plugin builds ignore it.
- **A tweak and a script rate would have emitted two rate lines.** The override wins and
  exactly one line is written; the verifier warns that a kind-global speed multiplier has
  taken over a value the script set for one spawn.
- The rate is spelled `2`, not `2.0` — `LAST_EFFECT_SET_RATE<F: ToF32>` — so the emitter and
  the write-back put identical text in the file. `check_effect_values` now refuses a
  non-finite rate, which plain `to_string` would spell `NaN` and break the build.

**Known gaps, deliberately left:**

- A rate that could not be attached is dropped by the export, because the effect export
  regenerates from `EffectCall`s and drops every unmodelled line already. Not specific to
  rate; fixing it means teaching the effect export to preserve `Raw`, which is its own task.
- The live capture stream dedupes on `(motion, kind, frame, func, args)`. Two spawns on one
  frame with the *same* rate collapse to one rate line, so the second spawn captures with no
  rate. Conservative — a missing rate, never a wrong one — and it only affects live capture.

**For whoever takes C1:** the attachment rule and its three implementations are the reusable
part; copy them rather than re-deriving. C1 carries the measured table for the rest of the
family, including one member smash-script never wrapped — read it before writing a slot table.

## Hitbox families

### [x] B1 — `ATTACK_ABS` (throw and swallow damage) (done 2026-08-04)

**The old title and premise were wrong, and this is the finding.** This entry called it
"absolute-position hitboxes" and said positions are world-absolute rather than bone-relative,
with a definition of done of "it draws in the viewport at the right place". Read the signature:

```rust
pub unsafe fn ATTACK_ABS(agent, kind: i32, id: u64, damage: f32, angle: u64, kbg: i32,
    fkb: i32, bkb: i32, hitlag: f32, unk: f32, facing: i32, unk2: f32, unk3: bool,
    effect: Hash40, sfx_level: i32, sfx_type: i32, _type: i32)
```

**There is no position, no size, and no bone.** It is not a spatial volume at all — it is the
damage/knockback definition applied to an opponent who is *already* caught, and the "absolute
kind" slot is what it applies to. Every one of the 32 corpus calls is in a throw or a Kirby
inhale (24 files, all `kirb*`/`doll*`), and the kinds are `..._CATCH` (22) and `..._THROW`
(15). So it cannot draw in the viewport, and the old "done when" was unreachable. The bone
dropdown does need suppressing, but for a different reason than the entry gave.

Still true from the earlier measurement: 16 arguments, all 32 occurrences, one arity, and
`macros::ATTACK_ABS` is declared — so no optional-shape detection is needed.

- **Trap, and it is the expensive one: the kind slot takes fighter-specific constants.** The
  corpus has `FIGHTER_DOLLY_ATTACK_ABSOLUTE_KIND_FINAL` alongside the two common ones. A
  `ConstTable` of just `CATCH`/`THROW` would fail to round-trip Terry's final smash, so the
  kind must be carried as written and offered as a dropdown that *accepts* unknown names
  rather than one that replaces them.
- **Slots 9, 11 and 12 are invariant across the whole corpus** (`1.0`, `0.0`, `true`) and are
  undocumented in `macros.rs` beyond `unk`/`unk2`/`unk3`. Carry them verbatim; do not expose a
  control whose meaning would be a guess. Slot 2 (`id`) is `0` in all 32.
- **Work order:** its own struct and slot table. The hit-property block genuinely does map —
  damage, angle, kbg, fkb, bkb, hitlag, `ATTACK_LR_CHECK_*`, `collision_attr`, sound level and
  attr, and `ATTACK_REGION_*` are all the same fields `ATTACK` has — so it can ride the
  existing `Hitbox` display type with geometry hidden, the way C3's colour commands ride the
  effect list. Do not reuse `ATTACK`'s *slot indices*; the layout is different.
- **Done when:** it round-trips the corpus oracles, shows in the hitbox list with geometry
  suppressed rather than zeroed, and syncs values back into the user's source.

**Two more traps found while doing it, both about the shared `ATTACK` prefix and both live
bugs rather than hypotheticals:**

- **The capture path read `ATTACK_ABS` through `ATTACK`'s 36-slot table.**
  `hitboxes_from_captures` bucketed on `func.starts_with("ATTACK")`, so a throw captured live
  had sixteen arguments read against thirty-six and came back as a plausible, wrong hitbox.
  The plugin's `is_collision_func` had the same prefix test and armed the clear-all gate for a
  call nothing clears. **Grep for `starts_with("ATTACK")` — and any other prefix bucketing —
  before adding a family whose name extends an existing one.**
- **The kind cannot be decoded back from a captured number, and must not be.** `lua_const` has
  93 `..._ATTACK_ABSOLUTE_KIND_*` constants in one namespace with heavy value collisions:
  `FIGHTER_ATTACK_ABSOLUTE_KIND_THROW` is `0x0`, and so is
  `FIGHTER_DOLLY_ATTACK_ABSOLUTE_KIND_FINAL` and most other fighters' finals. A captured `0`
  has no recoverable name. It is kept as the number, which `const_expr` writes back bare and
  which still compiles. This is the `HIT_STATUS_MASK` collision from B4 again, and worse —
  there, excluding the masks fixed it; here no table can be built at all.

Verified: 302 tests, clippy clean, `build_check.sh`, plugin builds. Both corpus oracles cover
all 32 calls. Live rules and hooks are built but not exercised against a running game.

### [x] B2 — `ATTACK_FP` (fighter-position hitboxes; done 2026-08-08)

The 461 locally cached scripts still contain zero `ATTACK_FP` calls. A larger
`SSBU-Dumped-Scripts` snapshot is the usable corpus oracle, however: its standard `smashline`
tree contains exactly one dumped call,
`lua2cpp_demon/demon/AttackStep2fHitShield.txt` (snapshot `59004685238a3f6f93bae905d3a4e079701c40b4`).
The source call has all 41 arguments and uses the symbolic
`*COLLISION_SITUATION_MASK_G` situation slot; the real-call round-trip test covers that case.

The complete 41-slot payload is retained as typed/raw data through parse and IR, project export,
live capture and rule injection, and source write-back. The panel exposes only established shared
hit properties; fighter-position geometry and undocumented fields are preserved but not guessed
or drawn as ordinary bone-local volumes. Export and source-sync use a separate `ATTACK_FP` slot
table, and the parser/export, live-capture/injection, source-sync, and real-dump tests cover the
five surfaces. Live hardware/game execution remains unexercised, as with the other built live
surfaces; it is not used as a substitute for source evidence.

### [x] B3 — Post-hoc hitbox tuning (done 2026-08-04)

These modify hitboxes *already out*, so they are edits to an existing collision rather than
new collisions.

**Measured after A3** — all have `macros.rs` wrappers, so no export trap:
`ATK_SET_SHIELD_SETOFF_MUL` 9, `ATK_HIT_ABS` 6, `WHOLE_HIT` 6, `ATK_POWER` 2,
`ATK_LERP_RATIO` 0. B4's "take that first" deferral is discharged — B4 shipped 2026-08-04.

**One member arrived late, from B5 (2026-08-04): `SET_SEARCH_SIZE_EXIST(agent, id: u64,
size: ToF32)`.** B5 listed it as a detection box; its signature says otherwise — it re-sizes a
search box *already out*, keyed on that box's id, which is precisely this entry's shape. It
would drop into `ATTACK_MOD_COMMANDS` and `AttackModKind` with no new machinery, and its
`ToF32` slot carries the same integer-formatting trap `attack_mod_num` exists for. It is
**not done**, and the reason is the one that governs B2: **zero corpus calls**, so there is
no vanilla line to prove the round trip against. Take it if a corpus with one ever appears.

**[x] `WHOLE_HIT` was not one of these and is done, as B4's family (2026-08-04).** It takes a
single `hit_status: i32` and the corpus writes `*HIT_STATUS_XLU` (5) and `*HIT_STATUS_NORMAL`
(1) — that is how the fighter *receives* hits, not an edit to a hitbox already out. It is the
all-bones sibling of `HIT_NODE`/`HIT_NO`, sharing the four-state `HIT_STATUS` `ConstTable` B4
already built. So it needed **no new `ExcuteStmt` variant** — a third `HurtTarget` alongside
`Bone` and `Group`, and all five surfaces followed from that.

- **This is the second macro filed by its name rather than its signature** (after
  `COL_PRI`/`COL_NORMAL` in B4 — see the traps section). `WHOLE_HIT` reads like a hitbox word.
  The `lua_const` oracle is **silent** for all five of B3's macros — no `MA_MSC_CMD_*`
  constants exist for them — so here the signature and the corpus context were the evidence.
- **The whole cost was one asymmetry, and it recurs on every surface.** `HIT_NODE`/`HIT_NO` are
  *(target, status)*; `WHOLE_HIT` is *(status)*, its target being the macro name. So the status
  slot index is 2 in the write-back and 1 in the capture reader for the pair, and one less for
  this one. `HurtTarget::takes_target_argument` asks it once by name. Five places needed it: the
  arity table in `HURT_COMMANDS` (listing it at 2 would have excluded every real call from
  `hurt_sites` and shifted every later site ordinal), the emitter, the source write-back, the
  capture reader, and the plugin's override application.
- **The corpus oracles cannot see this class of bug, which is worth remembering.** An unparsed
  line survives an export verbatim as `Raw`, so a `WHOLE_HIT` the parser ignored would
  round-trip exactly as cleanly as one it understood — all three oracles passed *before* the
  parse arm was added. The assertion that has teeth is that a span comes out at all.
- **The plugin had a live corruption waiting.** `hurt_action` applied `hit_target` to slot 0 and
  `hit_status` to slot 1 unconditionally, bounds-checked only. Slot 0 is the *status* for
  `WHOLE_HIT` and the *priority* for `COL_PRI`, so a rule aimed at one macro could write into
  another's meaning. It now matches on a `HurtShape` per macro. Relatedly, `COL_PRI` keyed its
  rules on a bare `u64::MAX` as "the targetless one"; there are now two targetless members, so
  the sentinels are named constants (`HURT_KEY_COL_PRI`, `HURT_KEY_WHOLE`) mirrored on both
  sides of the wire.
- **The all-bones reach is deliberately not modelled**, and the README says so. In the game a
  `WHOLE_HIT` covers the bones a `HIT_NODE` names, so it arguably ends an open per-bone span.
  No vanilla script mixes them — all 6 occurrences stand alone — so there is nothing to
  calibrate a cross-target rule against, and inventing one would draw spans the script does not
  describe. Revisit if a real mixed example turns up.

That leaves B3 proper at **17 occurrences** — but they are not one family, and the work order
below was written for a shape only two of them have. **Re-measured 2026-08-04 against
`macros.rs`, which is the oracle `lua_const` cannot be here** (it has no `MA_MSC_CMD_*` constant
for any of the four):

| macro | signature | corpus | is it an `(id, value)` modifier? |
| --- | --- | --- | --- |
| `ATK_SET_SHIELD_SETOFF_MUL` | `(id: u64, val: ToF32)` | 9 | **yes** |
| `ATK_POWER` | `(id: u64, power: ToF32)` | 2 | **yes** |
| `ATK_HIT_ABS` | `(kind: i32, unk: Hash40, target: u64, target_group: u64, target_no: u64)` | 6 | no — no id slot at all |
| `ATK_LERP_RATIO` | `(ratio: ToF32)` | 0 | no — no id slot at all |

So **B3 is scoped to the two that share the `(id, value)` shape**, 11 occurrences. The other two
are not "the same idea at a different arity"; they take no hitbox id, so the whole premise of
attaching them to a hitbox does not apply. Each is written up below with why it is not being
done rather than left to look merely unfinished.

- **The signature settled what the corpus could not.** The old bullet was right that
  `ATK_SET_SHIELD_SETOFF_MUL`'s 9 calls are byte-identical `(agent, 0, 7)` and so cannot tell the
  id slot from the value slot — but `macros.rs` declares `id: u64, val: ToF32`, which names them
  outright. `ATK_POWER` then confirms it from the corpus side: its two calls are `(agent, 0, 10)`
  and `(agent, 1, 10)`, varying the *first* slot across two hitboxes that share a value. This is
  the third time the signature was the oracle after `lua_const` came back silent.
- **Work order:** model them as modifiers attached to the hitbox id they target, shown on the
  timeline at their own frame. Do not fold their values into the parent `ATTACK` — the export
  must re-emit them as separate calls at their own frames. The corpus shows both placements:
  kirby/Attack100Sub puts `ATK_SET_SHIELD_SETOFF_MUL(agent, 0, 7)` in the *same* `is_excute`
  block as its `ATTACK(agent, 0, …)`, while kirby/AttackLw4 puts `ATK_POWER` five frames after
  the `ATTACK` it retunes. Both must survive as separate calls at their own frames.
- **Trap — the value slots are `ToF32`-generic, and the corpus writes bare integers.** All 11
  calls spell the value `7` or `10`, not `7.0`. [`acmd::num`](src/acmd.rs) deliberately appends
  `.0` because most float arguments sit in slots *declared* `f32`, where a bare `6` is a type
  error. These two are not: `ToF32` is implemented for `i32`, so both spellings compile, and
  emitting `10.0` over a vanilla `10` is gratuitous diff noise on a line the user never touched.
  Needs its own formatter, documented against `num` so the difference is not read as an
  oversight.
- **Not done — `ATK_HIT_ABS` (6), carried verbatim as `Raw` and that is the honest answer.**
  All six are the identical line `ATK_HIT_ABS(agent, *FIGHTER_ATTACK_ABSOLUTE_KIND_THROW,
  Hash40::new("throw"), target, target_group, target_no)`, where the last three are **local
  variables**, not values. There is nothing to edit: a parser expecting numbers drops the line,
  and an emitter writing numbers back breaks the throw. It is also not a modifier — with no id
  slot it is closer to B1's `ATTACK_ABS`, whose family it shares a prefix with. `game_` export
  walks the tree and keeps everything, so these already survive an export untouched.
- **Not done — `ATK_LERP_RATIO` (0).** No id slot, so it is not this family, and zero corpus
  occurrences, so it fails the same bar B2 does: the definition of done needs a round-trip test
  built from a real vanilla call and there is none. Re-measure if more of the archive is fetched.

**Result (2026-08-04).** `ATK_POWER` and `ATK_SET_SHIELD_SETOFF_MUL` land on all five surfaces as
one `ExcuteStmt::AttackMod { kind, id, value }` — one variant with a discriminant, since the two
share an argument layout exactly. 325 tests green, all 11 vanilla calls verified to round-trip
**byte-identically**.

- **Point events, not spans, and that is the whole shape of the feature.** Every other family
  here resolves to a range because some macro takes it back. Nothing takes these back, so
  `to_attack_mods` has no end-frame pass and the panel draws a marker at one frame. Inventing a
  span would have drawn a range the script never wrote — the same call B4's `WHOLE_HIT` reach
  made, arrived at from a different direction.
- **Its own site numbering space, which is the sharp part.** Reusing `HurtSite` would have meant
  that adding an `ATK_POWER` to a move shifted the ordinal of every later `HIT_NODE`, retuning a
  different call than the one clicked. `is_attack_mod_stmt` / `count_attack_mod_stmts` /
  `next_mod_site` mirror the hurtbox machinery beside it, including the zero-iteration `for`
  rewind, and `the_two_families_number_their_sites_independently` pins the boundary from both
  sides. `HurtboxAccum` became `WalkAccum` because it now carries two families.
- **A category per macro, not one keyed by hitbox id.** The two members can legally name the
  same id in the same frame window, so a shared category would let an `ATK_POWER` rule fire on an
  `ATK_SET_SHIELD_SETOFF_MUL` call and write damage into a shield multiplier. `CAT_ATK_POWER` 5
  and `CAT_ATK_SETOFF_MUL` 6 make that unrepresentable rather than merely unlikely — the same
  lesson `HURT_KEY_WHOLE` records, applied before the bug instead of after it.
- **The float formatter is a second one on purpose.** `acmd::num` appends `.0` because most
  float slots are declared `f32`, where a bare `6` is a type error. These are `ToF32`-generic and
  every vanilla call writes a bare integer, so `attack_mod_num` keeps the integer spelling and
  the write-back uses `to_f32_edit` rather than `float_edit` to match. Without this every export
  would have rewritten 11 untouched lines. **No oracle would have caught it**: `verify_move`
  checks semantics, and `7.0` means what `7` means.
- **The corpus could not have settled the slot order** — all 9 `ATK_SET_SHIELD_SETOFF_MUL` calls
  are byte-identical. `macros.rs` declaring `id: u64, val: ToF32` is what did, with `ATK_POWER`'s
  two calls confirming it by varying the first slot alone. Third time the signature was the
  oracle after `lua_const` came back silent.

### [x] B4 — Hurtbox control (done 2026-08-04)

Intangibility and hurtbox state per bone. All five members land on all five surfaces:
`HIT_NODE` 30, `COL_NORMAL` 8, `HIT_RESET_ALL` 3, `HIT_NO` 2, `COL_PRI` 2 — 45 occurrences.
Every one has a `macros.rs` wrapper at the arity the corpus writes, so no A1-style trap.

Both entry traps turned out cheaper than written. The `ConstTable` already existed with both
directions ([param_labels.rs](src/param_labels.rs)), and the panel/lane split was a clean
addition rather than a restructure. The real cost was somewhere the entry did not look.

- **The trap that actually cost time, and it is C1's again in a new place.**
  `rebuild_script_from_hitboxes` retained only `ExcuteStmt::Raw` from the source, which was
  correct *only* because every non-collision line was `Raw`. Giving `HIT_NODE` its own variant
  therefore deleted every hurtbox line in a move the moment an unrelated hitbox was dragged.
  The filter is now an exhaustive `match` so the next variant is a compile error instead of a
  silent deletion. **Generalise this: before modelling any macro, grep for every place that
  filters on `Raw` or on a variant list, and make it exhaustive first.**
- **`HIT_STATUS_MASK_*` collides with `HIT_STATUS_*` numerically.** `MASK_NORMAL` is `0x1` and
  so is `INVINCIBLE`. `const_name` returns the first match, so a table built by prefix would
  label a live capture of an invincible bone as a mask. The table is exactly the four real
  states; `TERM` and the masks are excluded and the doc comment says why.
- **Design, and it differs from collisions on purpose:** hurtbox statements are *carried
  through* `state.script` rather than rebuilt from an edited list. The script is the model, so
  there is no second copy to drift, and `HurtboxState::site` is the ordinal that ties a
  resolved span back to the statement it came from. `hurt_stmt_mut` and `HurtboxAccum` are
  written to reproduce the same pre-order walk — including stepping the cursor over a `for`
  body whose count is zero, which is the one case where the two definitions could diverge.
- **Out of scope, deliberately:** the panel edits existing calls only. There is no "add a
  hurtbox state" button, and moving one to a different frame is not offered. Both are
  structure, which the export path can express and the source sync cannot; adding them means
  deciding where a new call goes in a block, which is the same open question as C6. A retime
  or a target-family swap made some other way is *reported*, not guessed.
- Verified: 296 tests, clippy clean, `build_check.sh`, plugin builds (2318336 bytes). The
  corpus oracle now runs `check_hurtbox_fidelity` over all 461 cached scripts, so every vanilla
  hurtbox call is known to re-export identically. The live surface is built but not exercised
  against a running game — that needs a deploy, which is not available here.

### [x] B5 — `SEARCH` / detection boxes (done 2026-08-04)

Grab-range and detection volumes. Geometrically these are close to grab boxes, so the panel
work is mostly reuse.

**Measured after A3:** `SEARCH` 7; `SET_SEARCH_SIZE_EXIST`, `ENABLE_AREA`, and `UNABLE_AREA`
appear **zero** times in the local corpus, though all four have `macros.rs` wrappers. So this
is really a one-macro task with three untestable extras — there is no vanilla call to build a
round-trip test from for those three, which the definition of done requires. Scope it to
`SEARCH` and say so, rather than shipping three families no corpus can check.

**Re-measured on the way in (2026-08-04), and the zero-corpus count was not the only reason
to drop the three extras — their signatures were.** Reading each, the way B3 should have been
read:

- `ENABLE_AREA(agent, kind: i32)` and `UNABLE_AREA(agent, kind: i32)` take **one int and no
  geometry at all**. They toggle an area the stage or the fighter already owns; they do not
  define a volume. "Geometrically close to grab boxes" cannot describe them — they are not a
  smaller `SEARCH`, they are a different thing, and they belong with the enable/disable
  lifetime commands (near **C4**), not in a collision panel. Filed there rather than here.
- `SET_SEARCH_SIZE_EXIST(agent, id: u64, size: ToF32)` re-sizes a search box that is *already
  out*. That is not a box command, it is the exact shape **B3** just built: a post-hoc
  modifier keyed on the id of a live collision. It also has the `ToF32` slot B3 found, so it
  carries the same integer-formatting trap. It belongs in B3's table, gated on a corpus call
  existing to test it against — which there is not. Recorded under B3, not done here.

So B5 is one macro, `SEARCH`, with 7 vanilla calls across 7 kirby scripts. The corpus does
exercise both arities (4 calls without the capsule end, 3 with it) and the extras genuinely
vary — `COLLISION_KIND_MASK_ATTACK`/`_HIT`, `HIT_STATUS_MASK_ALL`/`_NORMAL`, and an unnamed
int that is 0, 1 and 60 — so keeping them losslessly is doing real work, not defending
against a hypothetical.

`SEARCH` maps onto `Hitbox` better than `ATTACK_ABS` did: id, part, bone, size, x/y/z, the
optional capsule end, and all three of `situation_mask`/`category_mask`/`part_mask` land on
fields that already exist. The four with no home — collision kind, hit status, the unnamed
int, and a trailing bool — become a `SearchExtras` beside `CatchExtras` and `AbsExtras`.

- **Trap:** new collision family → it must be added to the plugin's `is_collision_func` and
  given a category id on the wire, alongside 0 attack / 1 grab / 2 wind.

**Result (done 2026-08-04).** All five surfaces. 335 tests, clippy clean, `cargo fmt --check`
clean, `build_check.sh` exit 0, plugin builds and the binary contains `hook_search` and the
`ATTACK/CATCH/SEARCH/WIND/CLEAR/HURT/ATKMOD` banner. Corpus at 460 files, so the oracles are
not vacuous. The live surface is built but not exercised against a running game.

- **Half the trap above was wrong, and reading the function said so.** `is_collision_func` is
  not "is this a collision" — it arms a gate that notes something is *out to be cleared*. A
  search volume is never cleared, exactly as `ATTACK_ABS` is not, and that comment is already
  in the function explaining why `ATTACK_ABS` is excluded. Adding `SEARCH` would have made the
  next `AttackModule::clear_all` look like it ended something. It is deliberately not added.
- **`SEARCH` is written in two shapes and the tail moves three slots between them.** Four of
  the seven vanilla calls omit the capsule arguments outright rather than writing `None`, so
  the mask arguments sit at 11..=13 in one and 8..=10 in the other. Each of the four surfaces
  needs its own discriminator, and **they are not the same test**: in source text a capsule
  slot is visibly a coordinate and the next argument is visibly a `*CONST`, but on the wire
  both are just numbers, so the live reader has to go by *argument count*. Testing the value
  there gave a 14-argument call a capsule of `[2.0, 1.0, 60.0]` — its own collision kind, hit
  status and undocumented int, bent into geometry. A test caught it; nothing else would have.
- **Two real bugs found on the way in, both older than this task.** Written up below.
- Not done, deliberately: `SET_SEARCH_SIZE_EXIST` (moved to **B3**), `ENABLE_AREA` and
  `UNABLE_AREA` (moved to **C4**). The local-cache measurement had no calls for the two area
  toggles; C4's later external-corpus measurement supersedes that evidence boundary.

### [x] B5a — The short-form `CATCH` bug B5 surfaced (done 2026-08-04)

Not a planned entry; found while measuring B5, because `CATCH` is dumped in the same two shapes
`SEARCH` is. Both halves read the status kind and situation mask from fixed slots 10 and 11.

- **Parsing** ran off the end of the token list on the short form and substituted the defaults,
  so all four short-form calls in the corpus — every one of them Kirby's inhale — came into the
  editor as an ordinary `CAPTURE_PULLED` grab instead of a swallow.
- **Writing** was worse: a capsule edit put `Some(1.0)` over the status constant, destroying the
  grab's behaviour and producing a file that does not compile. Now refused and reported.

**Why no oracle caught it, which is the part worth keeping.** `check_hitbox_fidelity` compares
the export against the *parsed model*, not against the original text — so a value lost on the
way in agrees with itself on the way out and the round trip is green. Both new tests assert
against the original text instead. Any future check of this kind needs the same shape.

### [x] B5b — Live rules were sent under the editor's category, not the plugin's (done 2026-08-04)

Also found on the way in, and it is why B5's live surface could not have been correct without
fixing it first.

The editor put `Hitbox.category` straight onto the wire. That works for attack, grab and wind,
where the two numbering spaces agree, and then stops: the editor's `CAT_ABS` is **3**, which is
the plugin's `CAT_HURT`. So **every live `ATTACK_ABS` edit since B1 has been read as a hurtbox
rule and silently done nothing** — the plugin listens for those on 4. A search box (display 4)
would have collided with `ATTACK_ABS` (wire 4) the same way.

Fixed with an explicit `game_link::wire_category()` that every rule-push site now goes through,
plus a test pinning each mapping and asserting neither family lands on the hurtbox category.

- **The lesson is the one this file keeps relearning, in a new place.** Two id spaces that
  agree on their first few members are not the same space, and the agreement is what hides it.
  It is the same shape as the site-ordinal counters in B3 and the per-family slot tables at the
  top of this file: *sharing a numbering space across families is the recurring bug here.*
- **Not verified against a running game.** The mapping is proven by test; that a live `ATTACK_ABS`
  edit now actually lands needs a deploy, which is not available here.

### [x] B6 — Site resolution disagreed with the walk about raw blocks (done 2026-08-05)

Three sibling counters in [data.rs](src/data.rs:1289) — `count_hurt_stmts`, `count_sound_stmts`,
`count_attack_mod_stmts` — exist for one purpose, at one call site
([data.rs:1634](src/data.rs:1634)): stepping the site cursor over a `for` body that runs **zero**
iterations, so a statement *after* the loop still gets the ordinal a plain pre-order walk of the
source would give it.

`count_sound_stmts` recurses into `AcmdStmt::RawBlock`, and its doc comment says why: *"`eval_stmts`
walks a raw block's body, so a sound inside one takes a site, and a count that disagreed with the
walk is the bug this function exists to prevent."* **The other two do not.** They return `0` for a
raw block, while [eval_stmts](src/data.rs:1644) recurses into it deliberately — a hitbox inside an
`if` has always been shown unconditionally, and eighteen `game_` scripts rely on that.

So a zero-iteration `for` containing an `if` containing `HIT_STATUS` / `COL_PRI` / `COL_NORMAL` /
`HIT_RESET_ALL` / `ATTACK_MOD` under-steps the cursor, and every **subsequent** hurtbox and
attack-mod edit resolves to the wrong line — producing a script that is still perfectly well-formed.
That is the failure mode the sound counter's comment names, in the two functions that did not get it.

**Closed 2026-08-05 — six arms added across four functions, all six pinned by mutation.**

**The bug was bigger than filed, and the reachability claim in the original entry was wrong.**
Both corrections came from checking, and they went in opposite directions.

- **Bigger:** it is not only the two counters. `hurt_stmt_mut` and `attack_mod_stmt_mut` — the
  *resolvers* — skipped `RawBlock` as well, and those need no zero-iteration loop to go wrong. A
  plain `if` around a `HIT_NODE`, with any statement after it, was enough: the branch's own site
  resolved to the call *after* the branch, and the last site resolved to nothing. The comment at
  [data.rs:1219](src/data.rs:1219) stated the opposite in as many words — that `RawBlock` was
  something "the two functions above do not have to handle" — and that sentence was the whole
  defence. It has been rewritten to say which exemption is real (`Bare`; no hurtbox is ever
  written outside an `is_excute`) and which was not.
- **Not reachable from vanilla, contrary to what this entry first claimed.** The filed figure —
  "5 statements across 3 files" — was an artifact: the measuring script treated the
  `unsafe extern "C" fn … {` header as a raw block, so *every* statement in *every* script counted
  as being inside one. Re-measured with the header excluded, the real counts inside a `RawBlock`
  are **hurtbox 0, attack-modifier 0**, against **sound 26 in 16 files**, **effect 107 in 12**,
  **hitbox 48 in 4**. That asymmetry is the entire explanation for the bug: `count_sound_stmts`
  got its arm when D1c's corpus demanded it, and the two families with no vanilla instance never
  did. **The first number a measurement gives you is worth one more minute of doubt when it is
  the number that justifies the task.**
- **Fixed anyway, and the justification is different from the one filed.** Not "vanilla does
  this" but "the resolver must mirror the walk, and a user's own script can compose two shapes the
  corpus only contains separately". That is a weaker warrant than a corpus call and a stronger one
  than B2's zero-evidence bar, which is about *inventing a macro signature* — here nothing about
  the game is being guessed. The fixtures compose a verbatim corpus `RawBlock` header with a
  corpus `HIT_NODE`; see the note on `HURT_IN_RAW_BLOCK` in [acmd.rs](src/acmd.rs).
- **The sound family had the code and never the test.** Deleting `RawBlock` from either
  `count_sound_stmts` or `sound_stmt_mut` left all 398 other tests green. The corpus oracle
  `every_corpus_sound_site_lands_on_a_call_of_its_own_macro` reads as though it covers this and
  cannot: it compares the walk against `acmd_src::sound_sites`, a **textual** scan, which is a
  different function from the IR resolver. Two implementations of "which call is site N", one
  oracle, one of them exercised. Both arms are pinned now.
- **Surfaces:** Parse+IR only, as filed. A site-numbering fix behind existing capability — no
  panel, live, export or write-back change. The panel benefits without changing:
  [app.rs:4653](src/app.rs:4653) and [app.rs:4515](src/app.rs:4515) are the two callers, and they
  were writing edits to the wrong statement.
- **Not verified against a running game**, and nothing here needs it: the defect and the fix are
  both in host-side site arithmetic.

## Effects

### [x] C1 — `LAST_EFFECT_SET_COLOR` and `LAST_EFFECT_SET_ALPHA`

Done 2026-08-04. All five surfaces; `cargo test` 282 green, both corpus oracles run for real
(461 files present), clippy clean, `build_check.sh` passes. The plugin was touched and builds;
the `diag.txt` stamp could not be checked, because `scripts/build.sh` only writes to
`target/output/` and deploying needs a running game.

Scoped on the way in to the two members with real corpus backing left after A3 took rate. Both
arities are uniform — `(agent, r, g, b)` 65 times and `(agent, a)` 4 times, no exceptions — and
both are declared in `macros.rs`. The remaining three are **C7**, below.

**The premise was wrong about where the value is, and that is the finding.** The entry said 33
of the 65 colour calls were being deleted by exports and that C1 would recover them. Modelling
the macro recovers **one**. Measured after the fact over the whole cache: `tint` binds on 1 call,
`alpha` on 4, and **64 of the 65 colour lines bind to nothing.** They look like this, and this
shape is nearly the whole corpus:

```rust
if macros::is_excute(agent) {
    macros::EFFECT_FOLLOW_ALPHA(agent, Hash40::new("dolly_roll_l_color1"), ...);
}
if(0x2508e0(*FIGHTER_INSTANCE_WORK_ID_INT_COLOR, 0)){
    if macros::is_excute(agent) {
        macros::LAST_EFFECT_SET_COLOR(agent, 0.146, 0.205, 0.333);
    }
}
```

A costume check between the spawn and its recolour. At runtime the tint does apply — the game's
"last effect" outlives the block — but it applies **on one costume**, so binding it would export
a costume-specific colour as an unconditional one. Refusing is correct, and the fix is not a
looser anchor. It is **C6**: carry the conditional through. Do not "fix" this by reaching further
back; A3 settled that rule and it is still right.

**The near-miss worth knowing about, because the next family will hit it too.** Modelling a
macro moved 32 lines *out* of C5's dropped-line report without moving them into the export: they
stopped parsing as `Raw`, so `unexportable_effect_lines` no longer saw them, and they were
discarded on the way to `EffectCall` with nothing anywhere saying so. Modelling a macro made the
loss quieter. `EffectScript::to_effect_calls_reporting_losses` now returns the modifiers that
bound to nothing, from the same walk that resolves the calls, so there is one implementation of
the anchor rule and no way to drop a line without naming it. `check_dropped_lines` reports both
kinds together. **Any future `LAST_EFFECT_SET_*` must return its unbound lines there** or it will
repeat this exactly.

**What did land, and is worth having regardless of the corpus count:**

- **The tweak-versus-script conflict is resolved.** `emit_effect_move_fn` was already writing
  `LAST_EFFECT_SET_COLOR` for a live colour multiplier while the parser had no variant to read
  one back — so a script's own tint was deleted and a tweak's survived, invisibly, because both
  sides of `check_effect_fidelity`'s comparison had already lost the line. Now the two reconcile
  the way A3 made rate reconcile: the override wins, exactly one line is written, and the
  verifier warns that it replaced the script's.
- **Panel, live, and write-back** are coherent for both macros. The plugin hooks
  `LAST_EFFECT_SET_COLOR` / `_ALPHA` for capture and override (33 hooks now, was 31), the wire
  carries `tint` / `alpha` as `Option` + `skip_serializing_if`, and `spawn_and_rate_sites` became
  `spawn_and_modifier_sites` — one scan for all three, sharing `modifier_edits`.
- **A modifier no longer ends the run for the modifier after it.** A tint followed by a rate is
  two lines about one spawn; the parser, the capture reconstruction, and the write-back scanner
  were each changed to agree on that, and `MODIFIER_COMMANDS` in
  [acmd_src.rs](src/acmd_src.rs) is the list to extend when a fourth arrives.

**Deliberate non-goals:**

- The tweak's fourth colour component is still ignored on export. It is the live form's alpha,
  which no panel exposes and which `live_tweak_from_override` does not test for identity, so
  emitting it would ship an opacity the user never set. Surfacing it is a live-override task,
  not this one.
- `num` for the tint and alpha slots, not the bare `to_string` the rate uses. Every one of the 65
  colour calls in the archive is written with a decimal point while its rates are whole numbers;
  matching each macro's own spelling is what keeps a re-exported vanilla script textually
  identical to its source.

### [!] C7 — The last four `LAST_EFFECT_SET_*` members (one evidence-blocked member and one runtime boundary remain)

What C1 left. Local cache counts and wrapper status, verified against the full `macros.rs`
declaration list. A separate public dumped-script corpus was checked read-only for shape and
coverage; its files are not part of this repository:

| macro | args | local cache | external oracle | `macros.rs` wrapper |
|---|---|---|---|---|
| `LAST_PARTICLE_SET_COLOR` | 3 (`ToF32`) | 1 | 685 / 254 files | yes |
| `LAST_EFFECT_SET_WORK_INT` | — | 1 | 105 / 70 files | **NO** |
| `LAST_EFFECT_SET_SCALE_W` | 3 (`ToF32`) | 0 | 1 / 1 file* | yes |
| `LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT` | 1 (`ToF32`) | 0 | 204 / 121 files | yes |

The external `LAST_EFFECT_SET_SCALE_W` hit has one argument rather than the three-argument
wrapper shape, so it is not safe evidence for a typed editor field. The offset member is the one
slice taken from that oracle so far.

**Boundary added 2026-08-08, without claiming C7 complete.** Remaining evidence-bounded C7 lines
inside an `is_excute` block were already carried by C6; bare forms now take that same frame-residue
path instead of becoming statement-level losses. `LAST_EFFECT_SET_WORK_INT` is now typed through
the parser/IR, editor, capture reconstruction, generated export, and source write-back: because
`smash-script` has no wrapper, the exporter emits a local helper over the linked
`sv_animcmd` primitive. The helper preserves authored Work ID tokens, while live capture records
the resolved integer only and live retiming/swap overrides stay explicitly unsupported until a
portable symbolic-to-runtime mapping is verified. The valid three-value
`LAST_PARTICLE_SET_COLOR` shape is typed below, but the local `SetInkColor` call still uses the
dump's zero-argument spelling after three preceding `WorkModule::get_float` stack inputs; that
malformed form remains explicitly carried. `LAST_EFFECT_SET_SCALE_W` still has only a malformed
one-argument external hit, so C7 remains open for evidence and runtime-boundary work.

#### [x] C7a — `LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT` (completed 2026-08-08; live runtime unverified)

The one-argument wrapper shape and 204 external calls were enough to take this member through
all five surfaces: `EffectCall::camera_offset` in the parser/IR, an optional Camera offset row in
the effect panel, per-spawn JSON and Skyline hook/capture handling, generated ACMD export, and
value-only source write-back for an existing modifier line. Adjacent modifiers remain bound to
their own spawn, and old projects deserialize with no authored value through `serde(default)`.

Offline regression coverage includes source parse/export read-back, source write-back, live
capture reconstruction, wire-field parity, and a standalone plugin release build. No emulator,
game, or UI automation was run, so live in-game behavior remains unverified.

The remaining member stays separately bounded:

- `LAST_EFFECT_SET_SCALE_W` has only the malformed one-argument external hit, not the measured
  three-argument wrapper shape.

#### [x] C7b — `LAST_PARTICLE_SET_COLOR` (completed 2026-08-08; live runtime unverified)

The measured three-value wrapper shape now has its own `EffectMacro` and `EffectCall::particle_tint`
field, separate from effect tint because the game targets the last particle rather than the last
effect. It has an optional Particle tint row in the effect panel, capture reconstruction, a
per-spawn JSON rule and Skyline primitive hook, generated ACMD export, and value-only source
write-back for an existing `LAST_PARTICLE_SET_COLOR` line.

The malformed local `SetInkColor` zero-argument line remains carried residue; it is not padded
with values from the preceding `WorkModule` stack operations. Offline regression coverage checks
typed parsing, malformed preservation, export read-back, source write-back, capture binding, and
wire/plugin field parity. The plugin release build passes. No emulator, game, or UI automation was
run, so live in-game behavior remains unverified.

#### [x] C7c — `LAST_EFFECT_SET_WORK_INT` (completed 2026-08-08; symbolic live mapping bounded)

The external corpus supplies a consistent two-argument source form, and the linked Skyline
primitive supplies the missing runtime operation even though `smash-script` has no wrapper. The
authored Work ID is now a typed `EffectCall::work_int`, shown in the effect panel, reconstructed
from live capture, emitted through a generated local helper, and value-editable in existing source
lines. The plugin records the runtime integer but does not reinterpret or override it; retiming or
swapping a Work ID-bearing spawn reports the limitation instead of guessing a runtime slot.

Offline tests cover source parse/export read-back, source write-back, capture binding, verifier
fidelity, and the generated helper. No emulator, game, or UI automation was run, so the runtime
symbolic mapping remains unverified.

### [x] C2 — Sword trails (done 2026-08-05)

`AFTER_IMAGE4_ON` / `_arg29` / `AFTER_IMAGE_ON`. Read-only today: the trail shows on the
timeline with its graphic and joint, and write-back explicitly refuses it
([acmd_src.rs:803](src/acmd_src.rs:803)) because a trail has no transform.

That refusal is correct and must stay. But a trail *is* placed — by the joints it names,
arguments 4 onward. Those are editable; the transform never will be.

- **Work order:** expose the joint pair and the trail parameters. Keep refusing transform
  writes, with the existing message.
- **Done when:** changing a trail's joints rewrites those arguments and nothing else, and
  dragging a position still reports the existing "no position arguments" skip.

**Measured on the way in (2026-08-04). The joint half cannot be done, and measuring found a
worse problem underneath it.**

- **Every trail-ON macro has zero corpus calls.** `AFTER_IMAGE4_ON` 0, `AFTER_IMAGE4_ON_arg29`
  0, `AFTER_IMAGE_ON` 0. Only `AFTER_IMAGE_OFF` appears, 4 times. So there is no vanilla call to
  build the round-trip test the definition of done requires — the same bar **B2** fails on.
- **Two of the three names do not exist.** `smash-script` declares only
  `AFTER_IMAGE4_ON_arg29`, `AFTER_IMAGE4_ON_WORK_arg29` and `AFTER_IMAGE_OFF`. There is no
  `macros::AFTER_IMAGE4_ON` and no `macros::AFTER_IMAGE_ON`, so an export emitting either names
  a function that does not exist — exactly the `AREA_WIND_2ND` trap `WIND_MACRO_COMMANDS`
  already documents. The parser accepting those names is *fine* (a source file may contain
  them and they ride through verbatim); synthesising one would not be.
- **The `_arg29` fixture in the tests is fabricated and has the wrong shape.** The real
  signature is 29 arguments with the second joint at slot **8**, not slot 5, and slots 5..=7
  are `trail_x1/y1/z1`. The fixture writes a `Hash40` where a coordinate goes. Any joint-pair
  work built on that fixture would have been built on a call the game never makes. **Do not
  reuse it — replace it if this is ever picked up with a real call in hand.**

So the joint pair is deferred, not refused: it is a reasonable feature with no way to verify it
today. Take it if a corpus with a real trail call ever appears, and read the signature first.

**A real trail-ON call does exist in the corpus, and it is none of the three names above
(found 2026-08-05 by C6b's loss audit).** Four occurrences, two scripts:

```
kirby/SpecialHi2.txt:39, :54      effect(*MA_MSC_CMD_EFFECT_AFTER_IMAGE3_ON, …)
kirby/SpecialAirHi2.txt:39, :54   effect(*MA_MSC_CMD_EFFECT_AFTER_IMAGE3_ON, …)
```

- **`AFTER_IMAGE3`, not `AFTER_IMAGE4` or bare `AFTER_IMAGE`.** A variant this entry did not
  know about.
- **It is written in the raw `effect(*CMD, …)` form, not as a `macros::` call** — which is
  consistent with, and explains, the finding above that the wrapper names do not exist. The game
  reaches the command through the `lua_const` id. So the shape of any editing support here is
  the raw command form, and anything built around `macros::AFTER_IMAGE4_ON` is still building on
  a function that is not there.
- **27 arguments after the command id**, opening `Hash40("tex_kirby_cutter")` twice, then `12`,
  then `Hash40("haver")` — so the joints are hashes and there are three of them, not the pair
  this entry assumed. Read the real line before designing anything; **do not reuse the
  fabricated `_arg29` fixture**, which the bullet above already condemns.
- Two of the four are in the loss list (each script carries one and drops the other), so this is
  also live evidence for the C5 report. **It would not move the ratchet, though**: `pop()` is in
  these same two files and nowhere else, so `SpecialHi2` and `SpecialAirHi2` stay lossy either
  way. 20 dropped lines would become 18; 15 lossy scripts would stay 15. Check that before
  proposing this as a way to bring the count down.

That is the "real call in hand" this entry was waiting for. It does not make the joint-pair work
*done*, and it changes what that work is — but it is no longer blocked on evidence.

**Done 2026-08-05 — the raw-command trail is modelled end to end.** All four vanilla trail-ON
calls now parse, export byte-identically, and take a joint edit. What the entry got wrong, and
what the work found:

- **26 arguments after the command id, not 27.** The note above counted the id itself. An
  off-by-one here shifts every slot, which is the whole hazard the `_arg29` bullet warns about,
  so it is corrected rather than left as a stale number.
- **The layout is `AFTER_IMAGE4_ON_arg29`'s, three arguments short** (no `cull`, `unk16`,
  `unk17`). That is not assumed from the name: eight independent positions agree by *type* —
  `Hash40` at 1, 2, 4, 8, 13, 14; `u64` at 3; `bool` at 12; and the two `i32` `lua_const`s at 23
  and 25. So the "three joint hashes" above are `trail_bone1` (4), `trail_bone2` (8) and
  `flare_bone` (14) — the pair this entry originally assumed is real, and the third is the
  flare's bone, a different thing.
- **The command id sits in the slot `agent` occupies in a wrapper call**, so `TRAIL_GRAPHIC_SLOT`
  and `TRAIL_JOINT_SLOT` address both forms unchanged.
- **The parser and the write-back scanner had to be extended together.** `call_macro_ordinals`
  counts the calls the parser makes and `rewrite_effect_calls` indexes sites by that ordinal, so
  typing a line the scanner cannot see would not fail — it would splice a later edit into a
  *different* call. Both now go through `data::RAW_TRAIL_COMMANDS` and the same
  `acmd_src::raw_trail_line`, and a corpus oracle asserts the two lists agree across all 461
  scripts.
- **A real bug fell out, invisible until trails parsed.** `AFTER_IMAGE_OFF` resolved against the
  most recent open *call*, not the most recent open *trail*. Kirby `SpecialHi2` opens a trail and
  an `EFFECT_FOLLOW` in one `is_excute`, so the close landed on the follow: the `AFTER_IMAGE_OFF`
  vanished (exported trail runs forever) **and** the follow gained an end frame the script never
  wrote, so the export invented an `EFFECT_OFF_KIND` killing an effect early. With no trail call
  in the model the wrong answer had been the only answer. A close with no local trail — kirby
  `SpecialHi4` ends what `SpecialHi2` began, two scripts, one move — is now carried verbatim
  instead of dropped.
- **The loss ratchet moved further than predicted: 15 scripts → 13, 20 lines → 16.** The bullet
  above expected 18/15 because `pop()` sits in the same two files. It was wrong in the useful
  direction: each `methodlib::L2CAgent::pop()` shares its `is_excute` block with a trail and had
  no spawn to ride on until the trail became one, so both went too. `SpecialHi2` and
  `SpecialAirHi2` are now clean. The audit's composition table and the `lossy <= 13` ratchet are
  updated; `mod_export`'s reloaded-loss test had to be repointed off `SpecialHi2` — its own guard
  said so rather than passing quietly — onto kirby `Run`, which loses a `wait_loop_sync_mot` the
  export drops on purpose and so will not churn again.
- **Seven mutations run; two survived first time and are the finding worth keeping.** Moving the
  graphic read to slot 2 and the joint read to slot 8 both passed all 388 tests, because every
  vanilla call writes the same `tex_kirby_cutter` at slots 1 and 2 and the same `haver` at 4, 8
  and 14. **The corpus cannot distinguish the slots the editor reads.** The fixture that closes
  this keeps the corpus's 26-argument layout and varies only the twin slots' values, with the
  declaration as the independent evidence for which is which.

**Named exceptions.**

- **Live is untouched.** Trails have no transform and the plugin has no trail primitive; nothing
  was sent or captured, and nothing here was verified on hardware.
- **The second trail joint (slot 8) and the flare bone (slot 14) are parsed past, not exposed.**
  The panel still shows one joint. Editing `trail_bone2` is the remaining half of this entry's
  done-when and is now cheap — the slot is addressable and the round-trip proven — but it needs
  a panel field and a `retarget_trail_line` slot, so it stays open below.
- **`MA_MSC_CMD_EFFECT_AFTER_IMAGE2_ON` is deliberately not modelled.** `lua_const` declares it,
  the corpus never calls it, so its layout is unverified; adding it to `RAW_TRAIL_COMMANDS` would
  claim slot 1 is a texture on the strength of nothing. It rides through verbatim, as today.
- **`macros::AFTER_IMAGE4_ON`/`AFTER_IMAGE_ON` parsing is unchanged and still unexercised by the
  corpus.** The fabricated `_arg29` test fixtures remain; they are not evidence and no new work
  was built on them.

**Closed 2026-08-05 — `trail_bone2` is editable and the done-when is met.** A trail's two joints
now parse from slots 4 and 8, show as `Bone 1` / `Bone 2`, rewrite exactly their own arguments on
export, and are reported rather than written when syncing into the user's own source. The
transform refusal at [acmd_src.rs:803](src/acmd_src.rs:803) is unchanged and still fires.

- **The `_arg29` fixture this entry twice told its successors not to reuse was still in the
  tests, and reading slot 8 turned it from dead weight into a wrong answer.** It put `sword2` at
  slot 5 — a `Hash40` where `trail_x1` goes — and `0.75` at slot 8, so the moment the parser
  looked there, the editor offered `0.75` as an editable joint. It is replaced with a
  29-argument call shaped by the declaration. The lesson is not "the warning was ignored": the
  warning *was* heeded, nothing was built on the fixture, and it was still load-bearing enough to
  produce a bug. **A wrong fixture is not made safe by nobody relying on it yet.**
- **A verification gap fell out, and it dates from C2's first half, not this one.**
  `check_effect_values` skipped a trail's graphic and joint names, reasoning that a trail's line
  rides through verbatim and is never re-quoted. That was true when written and stopped being
  true the moment `retarget_trail_line` began splicing edited names back in through `hash_arg`.
  Since then, typing a `"` into a trail's graphic or joint produced `Hash40::new("to"er")` — not
  Rust — with the verifier reporting nothing. All three names are checked now. **The comment
  justifying the skip is what went stale; the code around it still read as correct.**
- **Eight mutations, two survivors, both of them weak assertions of mine rather than dead code.**
  Dropping `trail_bone2` from `identity_matches` survived a test asserting the report contained
  `joint` — because the *other* skip message contains it too. And letting a call too short to
  reach slot 8 yield `Some("")` survived because nothing tested a truncated trail. Both now fail.
- **`identity_matches` is a message-quality guarantee here, not a safety one, and the test says
  so.** Both guards skip the call; only the wording differs. Recorded so nobody later reads that
  test as proof the source is protected — `differs`, a plain `!=` over the whole call, is what
  protects it.

**Named exceptions (the first half's, plus one).**

- **Live is still untouched.** The plugin has no trail primitive; nothing here ran on hardware.
- **The flare bone (slot 14) is parsed past, not exposed.** It is not a trail edge — it places a
  separate flare effect — so it is not part of "a trail's joints" and is deliberately out of
  scope. Nothing reads or writes it; it rides through.
- **`macros::AFTER_IMAGE4_ON` and `macros::AFTER_IMAGE_ON` get no second joint.** Neither name is
  declared by `smash-script` and neither appears in the corpus, so nothing says what sits at slot
  8 of a call that could not have been written. They still parse for round-trip only. Offering a
  field there would be guessing a layout from position — the trap this entry has now been caught
  by twice.

### [x] C2a — `AFTER_IMAGE_OFF` is exported without its argument (done 2026-08-04)

Found by the measurement above, and this one *is* testable — against 4 real corpus lines.

`AFTER_IMAGE_OFF<F: ToF32>(agent, unk: F)` takes one argument. The corpus writes
`macros::AFTER_IMAGE_OFF(agent, 0);` twice and `(agent, 3);` twice. `emit_spawn_stop` in
[acmd.rs:1940](src/acmd.rs:1940) emits `macros::AFTER_IMAGE_OFF(agent);` — **no argument at
all** — so every exported effect script containing a sword trail is a project that does not
build. The existing test asserts the wrong output, which is why it was never noticed.

- Why no oracle caught it: no corpus script pairs a trail ON with an OFF (there are zero ONs),
  so the emitter branch is never reached by the corpus round-trip. Coverage, not blindness.
- The `ToF32` slot carries B3's formatting trap: the corpus writes bare `0` and `3`, so this
  must not emit `0.0`. Reuse `attack_mod_num`.
- The author's own value has to survive, so `EffectMacro::AfterImageOff` needs to carry it.
  For a trail the editor ended itself there is no donor and a default is needed; the corpus
  splits 2/2 between `0` and `3`, so the default is a genuine choice and should say so.

**Result.** `EffectMacro::AfterImageOff` now carries `arg`, `EffectCall` carries `trail_off`,
and the emitter writes `macros::AFTER_IMAGE_OFF(agent, N);` through `attack_mod_num`.
`TRAIL_OFF_DEFAULT` is `0` and its doc comment says outright that it is a choice, not a
measurement. 337 tests, clippy clean, `cargo fmt --check` clean, `build_check.sh` exit 0.

- Three tests, each mutation-checked: reverting the formatter to `num` fails all three. The
  round-trip test covers **both** corpus values rather than one, because the archive does not
  agree on this argument and a test pinning only `0` would have let `3` be normalised away.
- **The old test asserted the broken output.** That is the finding worth carrying: a test can
  pin a bug as firmly as it pins a behaviour, and this one made the export look verified. When
  a fixture is hand-written rather than lifted from the corpus, it is evidence of nothing —
  and this file's fixture was hand-written *and* structurally impossible (see C2 above).
- Not a regression from any recent task; the bare call has been emitted since trails were
  first modelled. It survived because no corpus script pairs a trail ON with an OFF, so the
  round-trip oracle never reaches the branch. **Coverage, not oracle blindness** — the distinct
  failure mode from B5a, which the oracle ran over and still could not see.

### [x] C3 — Screen and body colour effects

Done 2026-08-04. All five surfaces; `cargo test` 273 green, both corpus oracles run for real
(461 files present). The plugin was touched and builds; the `diag.txt` stamp could not be
checked, because `scripts/build.sh` only writes to `target/output/` and deploying needs a
running game.

**The premise held — 69 occurrences, all six wrapped, the panel really is mostly a colour
picker — but the entry described the wrong problem.** This was filed as coverage: a family the
editor could not edit. It was worse than that. The effect export regenerates the whole function
from `EffectCall`s, and these parsed as `Raw`, so **every export silently deleted them**. A
mod built from kirby/AttackDash shipped with the burn colouring gone and nothing anywhere said
so. Same shape as A3's dropped rate, four times the usage, and the entry did not predict it
because it was reasoning about the panel.

**The measured argument layout, which is uniform across the family and was not obvious:**

| macro | args | corpus | shape |
|---|---|---|---|
| `FLASH` | 4 | 41 | r, g, b, a |
| `BURN_COLOR` | 4 | 10 | r, g, b, blend |
| `BURN_COLOR_FRAME` | 5 | 10 | **frames**, then the same four |
| `BURN_COLOR_NORMAL` | 0 | 5 | reset |
| `START_INFO_FLASH_EYE` | 0 | 3 | — |
| `FLASH_FRM` | 5 | 0 | **frames**, then the same four |

One layout for all six: the interpolation length comes first where there is one, and the four
colour components follow. That is measured, not assumed — the corpus pairs
`BURN_COLOR(agent, 2, 0.059, 0.008, 0)` with
`BURN_COLOR_FRAME(agent, 4, 2, 0.059, 0.008, 0.9)` on the very next line, which is the same
four values with a length pushed in front. So `color_slots` is one function rather than six
tables, and B1's warning about per-family slot tables does not bite here.

**`FLASH_SET_DIRECTION` is a third instance of the A1 trap, and is deliberately not modelled.**
`sv_animcmd` has it, the corpus calls it 8 times (dolly `SpecialHiCommand` and
`SpecialAirHiCommand`), and `macros.rs` does **not** wrap it. Modelling it would mean either
emitting a macro that does not exist or blocking the export of two Terry moves that export
today. It stays an unmodelled line — which means it is still dropped on export, exactly as it
is now. See C5.

**How they are carried, and why:** a colour command is an `EffectCall` with `color: Some(..)`
and the command in `spawn_func`, sharing the effect list rather than getting one of its own.
Everything that list already does — reordering, disabling, undo, project save, write-back
ordinals, export grouping by frame — is what these need too, and a parallel list would be a
second copy of all of it to keep in step. The cost is that the spawn fields are meaningless on
such an entry, so every site that reads one checks `color` first. The sites that had to learn
this: `emit_spawn_call`, `export_spawn_downgrades`, `check_effect_values`,
`effect_call_display_name`, `is_spawn_macro`, `transform_matches`, the properties panel, and
`push_effect_rules` — where `effect_name_hash("")` would otherwise have keyed every colour
command in the game to one hash.

**Also landed:**

- **Live.** The plugin hooks all six: it records them, so a captured move comes back with its
  colouring, and rewrites their arguments when a rule retunes one. These name no effect kind,
  so a rule keys `eff_hash` on hash40 of the lowercased *command* name — no graphic is called
  `burn_color` — which reuses the whole motion/frame/suppress matcher unchanged. `color` and
  `transition` are `Option` + `skip_serializing_if`, so older plugin builds ignore them.
- **A command dropdown**, the same shape as A2's. Switching changes how many arguments the call
  takes, so the payload is reshaped to the new signature on the spot rather than left for
  `check_color_values` to block on.
- `check_color_values` refuses a payload whose shape disagrees with its command — that is a
  call the signature does not accept, so it is a blocker by the same rule as a wind command an
  argument short.

**Known gaps, deliberately left:**

- The live path can retune a command the script already calls; it cannot inject one the script
  never makes, and cannot move one to a different frame. Unlike a spawn there is no captured
  argument list to replay. Adding and retiming reach the export, not the preview.
- The colour picker clamps to 0..=1 but the drag fields do not, because the corpus writes
  `BURN_COLOR(agent, 2, …)` — an over-bright red a clamping editor would silently dim.

### [x] C5 — Name the lines the effect export deletes

Done 2026-08-04. `cargo test` 275 green, both corpus oracles run for real, clippy clean,
`build_check.sh` passes. Plugin untouched — this is verifier-only, and surfaces 1–3 and 5 are
unchanged by design: the point is to describe what surface 4 already does, not to change it.

Split out of C3, which found the general case behind its own symptom. Scoped down on the way
in to the *report* half only; the preservation half is now **C6**, below.

The effect export regenerates the function from `EffectCall`s, so any line without a typed
variant is deleted on export. That was true before C3, before A3, and is still true — the
change here is that it is no longer silent. [`unexportable_effect_lines`](src/acmd.rs) walks an
`EffectScript` for `Raw` statements and `Raw` macros, and `check_dropped_lines` in
[acmd_verify.rs](src/acmd_verify.rs) names each one under the generated-source pane.

**What the corpus actually loses.** Measured over the script cache, not estimated — 132 effect
scripts produce calls, and **32 of them (24%) lose at least one line.** By head of line:

| Count | Line | Note |
|---|---|---|
| 33 | `macros::LAST_EFFECT_SET_COLOR` | The biggest single loss. **C1 modelled it and this barely moved:** all but one of these are cut off from their spawn by a costume `if`, so they now bind to nothing and are reported as unbound rather than as unparsed. **C6 closed this to 0** — carried verbatim under its own costume check. |
| 62 | `if` / `if !WorkModule::is_flag` / `if get_value_float` / `else {` | Conditional effects export as unconditional. **C6 closed this to 5**, all of them nested guards. It also fixed a second bug hiding underneath: the parser flattened `if A { X } else { Y }` into sibling spawns, so a move exported as doing *both*. |
| 7 | `wait_loop_sync_mot` | A timing primitive with no `EffectStmt` variant. |
| 4 | `macros::LAST_EFFECT_SET_ALPHA` | **Fixed by C1** — all four bind and survive an export. |
| 4 | `methodlib::L2CAgent::pop…` | Not a call to model; genuinely script plumbing. |
| 3 | `EffectModule::req_screen…` | Direct module calls, no macro wrapper involved. |
| 2 ea | `FILL_SCREEN_MODEL_COLOR`, `CANCEL_FILL_SCREEN` | Screen-wide colour, adjacent to C3 but a different family — **this line was right and C6b's later bullet contradicted it**; both are E3's, at arity 12 and 2. |
| 1 ea | `LAST_EFFECT_SET_WORK_INT`, `COL_NORMAL`, two `EffectModule` calls | `LAST_EFFECT_SET_WORK_INT` is the A1 trap again (see C7). |

**Warning, not blocker — and that is the decision to revisit if it ever looks wrong.** A real
loss of the user's code arguably belongs in classes 1–3, which refuse the export. It is a
warning because 24% of vanilla scripts carry such a line: blocking would swap a lossy export
for no export, which helps nobody, and the message a user acts on is identical either way.
**C6 has since landed and the residue is 19 of 132, not 32.** The arithmetic behind this
paragraph is therefore out of date; C6b carries the re-decision. Do not re-quote the 24% figure.

- **Known gap, deliberate:** the **Export Mod Folder** path passes `None` and does not run this
  check. A saved project stores `effect_calls_full` — resolved `EffectCall`s and nothing else —
  so by export time the dropped lines are already gone from the data. Making the project carry
  them is the same plumbing C6 needs, so it lands there rather than being done twice. The
  generated-source pane, which is where a user looks before exporting, does have the script and
  does check. **Half closed by C6:** carried lines ride on the calls a project saves, so those
  do report here. The dropped half is C6c.
- **Also deliberate:** punctuation-only lines are filtered out. The emitter regenerates every
  brace it needs, so reporting one `}` per block would bury the lines that are a loss. The
  filter is "contains a letter or digit", which keeps `if … {` and `else {` in.

### [x] C6 — Carry unmodelled effect lines through the export (done 2026-08-04)

Done. `cargo test` 305 green, both corpus oracles run for real plus a new third one, clippy
clean, `build_check.sh` exit 0. Plugin untouched — see the live gap below.

**Result: the export drops lines from 19 of 132 effect scripts, down from 28.** Measured, not
estimated, and now asserted by `the_effect_export_still_loses_no_more_of_the_corpus_than_it_did`
so the number can only go down. C5's table said 32; the true baseline on `main` was 28, because
C1 and C3 had already moved some. What the remaining 19 lose:

| Was | Now | Line |
|---|---|---|
| 64 | **0** | costume `if(0x2508e0(…)){` headers |
| 33 | **0** | `LAST_EFFECT_SET_COLOR` — every one now carried under its own costume check |
| 12 | **0** | `if !WorkModule::is_flag(…) {` |
| 9 | **0** | `if get_value_float(… LR) < 0.0 {` |
| 8 | **0** | `FLASH_SET_DIRECTION` |
| 9 | 5 | `else {` — the survivors are inside a second conditional, reported by design |
| 8 | 8 | `COL_NORMAL` — in blocks with no spawn to ride on |
| 7 | 7 | `wait_loop_sync_mot` — **deliberate**, see below |
| 4 | 2 | `methodlib::L2CAgent::pop()` |

**The root cause, which C5 did not name.** The two exports are not built the same way.
[`emit_move_fn`](src/acmd.rs) emits a `game_` script from `script.stmts` — it walks the
statement tree, so anything in the tree survives whether or not the emitter understands it.
[`emit_effect_move_fn`](src/acmd.rs) regenerated an `effect_` script from a flat
`Vec<EffectCall>` grouped by frame, so **only what became a typed call could come out**. Every
loss in C5's table followed from that one asymmetry.

**Measured the corpus before designing** (`137` effect functions), because the shape of the
residue decides whether to fix the asymmetry or work around it:

| Measurement | Result | What it ruled out |
|---|---|---|
| Functions with a non-`is_excute` conditional | **13 of 137** | A narrow feature. Not worth rewriting the emitter for. |
| Maximum conditional nesting depth | **1** | No recursion. A guard is one string, not a stack. |
| `frame()` / `wait()` inside a conditional | **1** | Frame arithmetic does not branch, so frame-grouping survives. |
| Statements inside a guard | **only `is_excute` blocks and effect macros** | A guard always wraps whole blocks, so it can be re-emitted around one. |
| Brace-balanced function bodies | **137 of 137** | Verbatim passthrough of a block is safe. |

So the frame-grouped emitter stayed and learned to carry residue. A structure-preserving rewrite
would have put add, delete and retime — the editor's actual value — at risk to serve 13 files.

**What landed.** `EffectStmt::Cond` (the parser stops flattening conditionals),
`EffectCall::guard` (a spawn re-exports inside its own condition), and
`EffectCall::leading` / `trailing` (verbatim lines, wrapper regenerated). Panel shows them under
**Kept as written**; `check_carried_lines` warns that a copied line is not an understood one.

**Findings worth keeping:**

- **Position is load-bearing, so residue hangs off a call and not off a frame.**
  `LAST_EFFECT_SET_COLOR` recolours whatever spawned most recently, and dolly's `SpecialHiCommand`
  puts three spawns and 24 tints in one frame block. Anchoring residue to the frame would have
  emitted the spawns then the tints and landed all 24 on the third spawn — every line present,
  move wrong. Pinned by `each_costume_tint_stays_with_the_spawn_it_recolours`.
- **Flattening was hiding a second bug.** The parser turned `if A { spawn X } else { spawn Y }`
  into two sibling spawns, so a move that spawned one graphic facing left and another facing
  right exported as spawning *both*. `Cond` fixes that as a side effect of keeping the header.
- **`else` is not `else`.** In the dumps it attaches to `if macros::is_excute(agent)`, not to the
  outer conditional — [kirby/CapturePulledHi.txt:6](). The decompiler mis-scopes it and the
  function balances only by accident. A guard header is reproduced as opaque text and never
  interpreted; anything pairing an `else` with its `if` would be wrong on real input.
- **Loop unrolling duplicated the loss report** — `eval_effect_stmts` runs a `for` body once per
  iteration, so a line inside one was named four times. `COL_NORMAL` read 16 against a true 8
  until the walk started rewinding its residue state per iteration, exactly as
  `call_macro_ordinals` already did. Caught by comparing against a worktree at `main` rather than
  trusting the new number.
- **The doubling trap cannot fire.** C6's entry warned that re-emitting a preserved line that is
  itself an unrecognised spawn would double it. It cannot: a line only becomes residue if it
  produced no `EffectCall`, so a carried line and a regenerated call are never the same spawn.
  Asserted in `a_rate_with_no_spawn_directly_above_it_attaches_to_nothing`.
- **Timing primitives are deliberately still dropped.** `wait_loop_sync_mot` advances the
  coroutine, and the regenerated function states every frame absolutely with its own `frame()`
  calls. Carrying it would shift every effect after it — an export that compiles and plays wrong,
  which is worse than an honest deletion. All 7 stay reported.
- **Faithful preservation means preserving invalid Rust.** The dumps are decompiler output:
  `if(0x2508e0(…)){` will not compile. Carrying it through is still the right trade — a user can
  fix a line they can see, not one that was deleted — but it turns a silent wrong export into a
  build error, so `check_carried_lines` says so up front rather than letting `cargo` say it.

**Known gaps, deliberately left:**

- **Live preview does not carry these.** A carried line is text whose condition only the game can
  evaluate; the plugin replays captured calls and a capture already reflects the branch that ran.
  Export and the generated-source pane are the surfaces this task touches. The plugin is
  unchanged, and not by oversight.
- **C5's export-path gap is now half closed rather than closed.** Carried lines travel *on* the
  calls, so a saved project reports them without needing the script. Dropped lines still do not:
  that would need `FighterEdits` to store the loss list beside `effect_calls_full` — a schema
  change for a report, not for behaviour. See [acmd_verify.rs:161](src/acmd_verify.rs:161).
- **One guard deep.** A conditional inside a conditional would have to overwrite the outer one,
  and neither choice is right — keeping the outer is too permissive, keeping the inner is wrong
  in a different direction. Nothing in the corpus nests, so the outer is kept and the inner is
  reported as a loss. That is the 5 remaining `else {`.
- **Guarded spawns are stopped unguarded.** `EFFECT_OFF_KIND` is `kill_kind`, a no-op when
  nothing of that kind is live, so an unguarded stop for a guarded spawn is harmless — whereas a
  guarded stop would leave the effect running forever on the branch that skipped the guard.

### [x] C6b — The effect scripts the export still loses a line from (closed 2026-08-05 at 15)

C6 took the corpus from 28 lossy scripts to 19 and asserted the number, so this is what is
left. Read C6's result table first — it says exactly which lines these are. Nothing here is
large; the point of the entry is that the remainder has been *identified*, so it can be closed
deliberately rather than discovered again.

**`COL_NORMAL` is done (2026-08-04): 19 → 15, all 8 occurrences recovered.** The rest of this
entry is still open.

- **[x] `COL_NORMAL`, 8 occurrences — the biggest item, and the cheapest.** These sat in
  `is_excute` blocks that contain no spawn, so there was no call for them to ride on.

  **The entry's premise was wrong, and checking it first is what made this a one-row change.**
  `COL_PRI` and `COL_NORMAL` are *colour-blend* commands, not body collision: `lua_const` names
  them `MA_MSC_CMD_COLOR_BLEND_COL_PRI` and `MA_MSC_CMD_COLOR_BLEND_COL_NORMAL`, in a family of
  exactly six with `FLASH`, `FLASH_FRM` and `FLASH_OFF`. So `COL_NORMAL` is not merely "the same
  shape as C3" — it is C3's family, and the direct sibling of the `BURN_COLOR_NORMAL` already in
  `COLOR_COMMANDS`. One row in that table, no new type, and the panel, the emitter and the
  write-back all picked it up untouched because two argument-free colour commands already shipped
  through them.

  What it actually cost beyond that row:
  - **A plugin hook move, not an addition.** `sv_animcmd::COL_NORMAL` was *already* hooked, in
    `hitbox_viewer`, next to `COL_PRI` and recording only. Only one hook may replace a symbol, so
    the second one failed to link — which is the good failure. It now lives with the colour
    family in `effect_viewer::acmd_hooks`, records identically, and additionally honours
    suppression, so disabling a reset live is what it should be: the tint stays up.
  - **A duplication a `CaptureLine` cannot resolve.** Capture lines do not record which function
    ran them, so every builder filters the one stream by macro name — and two builders that
    accept the same name export it into two different functions off one live call. The tables
    happened to be disjoint until now. `hurtbox_script_from_captures` therefore takes
    `COL_NORMAL` only while a `COL_PRI` it saw is open, which is the one thing that side still
    needs it for. **Trap inside the trap:** the open/closed flag must be moved by that pair
    alone. Reading it off every statement lets the `HIT_RESET_ALL` that so often shares the reset's
    frame close the span first, and the existing capture test catches exactly that.
  - **`COL_PRI` deliberately left alone.** It is not in the loss list — both occurrences share an
    `is_excute` block with a `FLASH`, so C6 already carries them as that call's `leading`. Folding
    it into `COLOR_COMMANDS` needs a third payload shape, since it takes one integer rather than a
    transition or an RGBA. Do it only if something else forces a `ColorCall` field anyway.
  - **The `game_` side is now correctly *labelled* but still oddly *placed*.** All ten corpus
    occurrences of the pair are in `effect_` functions, so `ExcuteStmt::ColPri` / `ColNormal` has
    no corpus backing at all. Its plumbing is fine and the panel no longer calls the value a
    pushbox priority, but it still sits under hurtboxes. Moving it is a UI decision, not a
    correctness one — left for whoever next touches that panel.
- **`else {`, 5 occurrences — nested guards.** C6 keeps one guard per spawn and reports an inner
  one rather than overwriting the outer. Closing this means `EffectWalk::guard` becoming a stack
  and `EffectCall::guard` a `Vec<String>`. Cheap, but there is **no corpus case that exercises
  it correctly** — these 5 are all the mis-scoped `else` described in C6, so a test would be
  pinning decompiler noise. Do it only alongside a real nested example.
- **`wait_loop_sync_mot`, 7 — do not "fix" this.** It is dropped by decision, not by omission:
  it advances the coroutine while the regenerated function states every frame absolutely, so
  carrying it shifts every effect after it. The honest close is to *model* it as a timing
  statement that the frame walk understands, which is E2's neighbourhood (`FT_MOTION_RATE`),
  not this one.
- **[x] `methodlib::L2CAgent::pop()`, 2, and the bare `EffectModule::remove_screen` calls, 2.**
  Both gone, and neither by being modelled — the `pop()` pair went with C2's trail, and E3's
  frame-anchored residue took the two `remove_screen` calls. That was not predicted: this bullet
  said "genuine script plumbing with no editor meaning, worth leaving reported", and it was right
  about the meaning and wrong about the consequence. They were never lost because nothing
  understood them; they were lost because their frame block held no spawn to ride on, which is a
  property of where they sit and not of what they are. They are now copied through verbatim and
  warned about as verbatim, which is what "no editor meaning" should always have produced.
- **[x] `CANCEL_FILL_SCREEN`, 2 — belongs to E3, not here** (closed there 2026-08-06, and *not*
  by modelling the macro — see E3). **This bullet was wrong on both counts**
  (written 2026-08-04, corrected the same day by the arity check it told itself to do).
  It *is* wrapped in `macros.rs`, so it is emittable — that part held. But:
  - **C3 does not model `FILL_SCREEN_MODEL_COLOR`.** C6's own result table already said
    "adjacent to C3 but a different family", and this bullet contradicted it from memory.
    Nothing under `src/` touches the name. There is no family here to add a reset to.
  - **It is not `COLOR_COMMANDS`-shaped.** `CANCEL_FILL_SCREEN` takes `(i32, f32)` and
    `FILL_SCREEN_MODEL_COLOR` takes **twelve** arguments including an `EffectScreenLayer` and a
    screen priority. Neither is a transition-plus-RGBA, so a row in that table cannot hold them.
  - **The `lua_const` oracle is silent here.** There is no `MA_MSC_CMD_*_FILL_SCREEN` constant at
    all — only an unrelated `MA_MSC_EFFECT_FILL_SCREEN_LEGACY`. Worth knowing that the oracle
    answers for some macros and not others; when it does not, the signature plus the corpus
    context is the evidence.

  All 4 occurrences are in dolly's `FinalAirStart` / `FinalAirEnd` — full-screen final-smash
  staging, which is exactly what E3 describes, and E3 already lists both macros by name. Do it
  there, with the camera work, or not at all.

  **What E3 actually did: neither.** Every reason above for not modelling `CANCEL_FILL_SCREEN`
  still stands, and it is still not modelled. The 2 lines came back because the *placement*
  problem was fixed instead — a frame block with no spawn can now own its lines. Three bullets
  of this entry argued about which table to put a macro in when the defect was never the macro.

**Remaining after E3: `wait_loop_sync_mot` × 7 and `else {` × 5, in 12 of 132 scripts.** Both
are refusals with a written reason above, not omissions.
- **[x] The C5 finding stays a warning (settled 2026-08-05).** The old reasoning was an
  argument about frequency — a warning because a quarter of vanilla scripts tripped it, and
  under one in eight now. **Frequency turns out to be the wrong axis entirely**, and the three
  facts that actually decide it were all read out of the code rather than remembered:

  - **A blocker has no per-move granularity.** `has_blockers()` in
    [mod_export.rs:173](src/mod_export.rs:173) is a `bail!` over the whole verification, and
    `export_mod` turns that into an early return. One effect script that drops a line would
    therefore stop the export of every other move, every other fighter, and the EFF data — a
    project-wide failure caused by one line of one script.
  - **It would abort halfway, not before starting.** `source_project` is called at
    [app.rs:6239](src/app.rs:6239), and the EFF data mod is already on disk by then
    ([app.rs:6166](src/app.rs:6166)); `info.toml` is written *after*, at
    [app.rs:6283](src/app.rs:6283). So a blocker leaves a folder holding effect files and no
    `info.toml` — which ARCropolis will not load and which does not look like a failure. See
    **C6d**, which is this bug for the blockers that already exist. (Fixed there on 2026-08-05;
    the argument above stands on the other two points, and this one is now an argument about
    what a blocker *would* still cost — a project-wide refusal — not about debris.)
  - **Most of what is left is not the user's to fix.** Of the 20 remaining dropped lines, 7 are
    `wait_loop_sync_mot`, dropped by this tool's own decision, and 5 are the mis-scoped `else {`
    that C6b declined to pin. Refusing to export until the user fixes those is refusing over
    something they cannot edit and did not write.

  **And the argument *for* escalating was "a warning is not heard", which is a defect in the
  channel, not in the severity.** C6c gave it one; this change made it survive. Severity should
  describe whether the output is *wrong* — `source_project`'s own comment already draws that
  line, "a mod that will not compile, or that ships numbers other than the ones on screen". A
  script missing a `wait_loop_sync_mot` compiles, installs and plays. It is incomplete, not
  broken. **Do not reopen this on a frequency argument.**

**Measured remainder, 20 lines across 15 of 132 scripts** (`the_effect_export_still_loses_no_more_of_the_corpus_than_it_did`,
[acmd.rs:5937](src/acmd.rs:5937)): `wait_loop_sync_mot` 7, `else {` 5,
`effect(*MA_MSC_CMD_EFFECT_AFTER_IMAGE3_ON, …)` 2, `macros::CANCEL_FILL_SCREEN` 2,
`methodlib::L2CAgent::pop()` 2, `EffectModule::remove_screen` 2.

**The old version of that table listed 18 of the 20, and no test could tell.** The two raw
`AFTER_IMAGE3_ON` calls were missing from it. The ratchet asserted the script *count*, so a
table claiming to say exactly what is left was pinned to how much of it there was. The audit now
asserts the composition as a map, which is the assertion an entry like this one needs.

**Trap, paid for on the way to that:** the throwaway script that found the discrepancy split each
corpus file on `"\n}\n"` and parsed the bodies separately, and it reported 18 lines with
`remove_screen` absent — two mistakes that happened to cancel into the right-looking total. The
audit parses each file whole, which is what the export does. **A measurement that chunks the
corpus differently from the code under test is not measuring the code under test**; run the
question through the existing oracle instead of writing a second one.

**Nothing cheap is left in this entry.** Every remaining line is either deliberate
(`wait_loop_sync_mot`), untestable against this corpus (`else {`), genuine plumbing with no
editor meaning (`pop()`, `remove_screen`), another entry's work (`CANCEL_FILL_SCREEN` → E3), or
newly-arrived evidence for a different entry (`AFTER_IMAGE3_ON` → **C2**, which was deferred for
want of exactly this). Closed at 15.

### [x] C6c — Close C5's export-path gap (done 2026-08-05)

**The entry's premise was wrong, and this is the Rule 5 rewrite.** It opened "Carried lines
already report on the export path, because they travel on the `EffectCall`s a project saves."
They travel there, and `check_carried_lines` does run on every export, and the finding it
produces was **read by nobody**. `source_project` built the whole `Report` and consulted
`has_blockers()` — one bit of it. Every warning went on the floor, carried and dropped alike.

So the gap was not "one of the two halves is missing". It was that the export path had no
warning channel at all, and the half that was supposedly working was working into a void. That
also means the original scope note — "a schema change for a report, not for behaviour" — was
right about the schema and wrong about the stakes: it described the gap as biting only someone
who exports without opening the source pane, when in fact the source pane was the *only* place
any loss was ever reported.

**What shipped, in two halves that are both required:**

- **Somewhere to keep the list.** `FighterMod::effect_dropped_lines` (`#[serde(default)]`, keyed
  by move) beside `effect_calls_full`, mirrored by `AppState::effect_dropped_lines` keyed
  `"fighter/move"`. Written by `record_dropped_effect_lines`, called at each of the four places
  `state.effect_script` is assigned — the moment the script exists, and the only one, since the
  export regenerates the function from the calls and a line that became no call is in neither
  the calls nor the output.
- **A channel to report it through.** `source_project` returns `GeneratedSource { project,
  warnings }` instead of a bare project, and the export summary prints the warnings under the
  "verified" line. `Report::warning_summary()` caps at 5, on `blocker_summary`'s reasoning.

**Do not filter the loss list against the emitted text.** Tried, to stop a list carried in an
old project from naming a line a newer Visionary had learned to emit. Measured over the corpus
it silenced 2 of 20 losses: kirby's `SpecialHi2` and `SpecialAirHi2` each call
`methodlib::L2CAgent::pop()` **twice**, one carried on a spawn and one not, so the surviving
copy made the dropped one look reproduced. A stale warning is noise; a hidden loss is the
silence C5 exists to end, and the two are not worth trading. The reasoning is recorded above
`check_dropped_lines` so the next person does not re-derive it.

**Staleness is therefore a named limitation, not a solved problem.** A carried list says what
was lost when the move was last parsed. Opening the move re-derives it, and the source pane
always derives fresh, so it only goes stale for a move exported from a reloaded project without
ever being opened. That is the exact case this entry exists to serve, so it is worth knowing it
is served with a snapshot.

**Six mutations run; two survived and both were findings.**
- The project-save half was untested and a mutation deleting it passed everything — the same
  shape as `[[registration-is-the-half-that-does-nothing]]`: the reporting side was complete,
  correct, and reading a map the editor never filled. `project_loss_note` was extracted from
  `build_project` purely so it could be tested, and the editor's own source pane derives its
  copy from the open script, so an empty saved map looks identical from inside the app.
- The "only gather notes for moves being exported" filter in `source_project` could not change
  an outcome — `verify_export` walks the exported moves and asks the map about each, so an
  orphan note is never consulted. **Removed rather than tested.** The behaviour it claimed to
  protect is still asserted, at the layer that actually enforces it.

**Named exceptions.** Live is untouched and out of scope — this is a disk surface.

**~~Warnings still do not appear for the `.nro` build path, because that path does not call
`source_project`.~~ Both halves of that were wrong (corrected 2026-08-05 while closing C6b).**
There is one export path: `export_mod` calls `source_project`, prints its warnings, and then
spawns `cargo skyline build` on the source it just wrote — so the `.nro` path is the same path
and did print them. What it then did was *erase* them: the background build's completion message
assigns straight to `state.status`
([app.rs:12873](src/app.rs:12873)), which is where the export summary had put them. A
mod-folder export reported a dropped line for as long as the build took, then replaced it with
"Mod folder ready". The developer export, which does not build, was unaffected — so the erasure
hit exactly the path a normal user takes.

The fix is under C6b: the warnings now also go into the exported mod's `README.md`, and the
completion message re-states them on every outcome. **`state.status` is one line, and anything
in it is a matter of time** — treat it as a notification, never as the record.

**Trap for whoever adds the next verification check:** `Report` findings are only rendered in
the source pane. Anything new reaching only `verify_export` is invisible unless it is a blocker
or it goes through `warning_summary`.

### [x] C6d — A blocked export leaves a half-written mod folder (done 2026-08-05)

Found by C6b's warning-vs-blocker re-derivation, which had to establish what a blocker actually
does. It does not do what its own comment says.

`source_project` is introduced with "Nothing reaches disk until the generated code has been read
back and matched against the edits it came from." That is true of the *source*, and false of the
*folder*. `export_mod` writes the rebuilt EFF files at [app.rs:6166](src/app.rs:6166), calls
`source_project` at [app.rs:6239](src/app.rs:6239), and writes `info.toml` at
[app.rs:6283](src/app.rs:6283). A blocker `bail!`s in the middle of that and `export_mod` returns
straight away.

What is left behind is a directory holding `effect/fighter/…/ef_x.eff` and no `info.toml` and no
`plugin.nro`. ARCropolis will not load it, the status line says "Export failed" and is gone at
the next click, and `unused_export_root` means the *next* export goes to a differently-named
sibling folder rather than replacing it. So the debris accumulates and does not announce itself.

- **This is not hypothetical and not about C5.** It is what every blocker that already exists
  does today — a rounded hitbox value, a mismatched number, anything `verify_export` refuses.

**Fixed.** It was a move, as predicted. Verification now runs first and a refusal returns before
anything is written, so the folder the user picked is left exactly as it was found.

**The move needed a seam to be testable at all.** `export_mod` opens a native folder dialog and
takes `&mut self` on a type no test can construct, so ordering inside it is unobservable. The
writing half came out as a free `run_export(&ModProjectFile, &ExportInputs) -> Result<ExportOutcome, String>`
([app.rs](src/app.rs)), with everything it needed from the app — resolved `.eff` source paths,
the dump root, the destination, the source root — resolved into plain data first. **The decision
and the writes it guards had to land in the same function**; splitting them would have left the
ordering guaranteed by the order of two statements again, just in a different method.

`Err` now means "the destination is untouched". `ExportOutcome::errors` keeps its old and
different meaning — one file did not write, the rest did — which is the merge the original
watch-for warned against, and it did not happen.

- **The first test written for this was vacuous, and a mutation caught it.** It built the project
  with a junk `.eff`, so `rebuild_eff_bytes_for_slot` failed and nothing was written *for reasons
  having nothing to do with ordering*. Restoring the old verify-after-EFF order left all three
  tests green. A successfully rebuilt EFF is the only thing that ever reached disk ahead of the
  decision — `info.toml` and the README came after it in both orders — so a test that cannot
  produce one is testing nothing.
- **What replaced it is paired in a single test.** Half one exports the same project *without*
  the blocker and asserts the EFF file lands; half two adds the blocker and asserts the folder is
  empty. Half one is the guard: if the rebuild ever stops working, that half fails loudly instead
  of half two passing for free. With the old order restored, it fails on the right assertion.
- **Named limitation: that test is corpus-gated.** It needs a real `ef_mario.eff` and so skips
  unless `VISIONARY_EFF_ROOT` points at an extracted `effect/` tree, exactly like every other EFF
  test in the crate. The two ungated tests in the module cover the refusal and the accepted
  control, but **the assertion that specifically distinguishes the two orderings only runs with
  the corpus present.** Do not treat a green run on a bare checkout as having exercised it.
- **Deliberate: the folder dialog still opens before verification.** A refused export therefore
  still asks where to put it, and then puts nothing there. Verifying earlier would move the
  decision out of `run_export`, which is the one place a test can see it happen next to the
  writes — one dialog is worth less than that.
- **Three mutations run, all three caught**: the old verify-after-EFF order (caught by the paired
  test, and *not* by the vacuous one it replaced); swallowing the blocker with `unwrap_or(None)`;
  and never writing `info.toml`, which the accepted control caught.
- **Watch for:** the EFF loop pushes to `errors`, which is a different failure channel that
  *does* continue. Do not merge the two; a failed EFF write should still leave the rest.

### [!] C4 — Effect lifetime control (typed point controls landed; one runtime mapping remains bounded)

Detach interacts with the follow/off-kind lifetime the editor already models for spawns.

**Historical boundary:** the local cache had no calls for the remaining members, so the entry was
not schedulable from that cache alone. The external-corpus measurement below supersedes that
evidence boundary for source-shape work.

**Re-measured 2026-08-05: `SET_PLAY_INHIVIT` is not an effect command and never was.** Its
signature is `(agent, se: Hash40, unk: ToF32)`, its 10 corpus arguments are all sound-effect
labels (`se_kirby_dash_stop`, `se_common_dizzy`), and **all 10 calls sit inside `sound_`
functions.** It suppresses a sound effect, not an effect spawn. It belongs to D1 and has been
counted there. That left the local cache with `EFFECT_DETACH_KIND` 0, `EFFECT_DETACH_KIND_WORK` 0,
`ENABLE_AREA` 0 and `UNABLE_AREA` 0 at the time. The external-corpus remeasurement and C4a
fixture above now provide the source-shape evidence needed for the typed implementation.

**Re-measured 2026-08-08 against the read-only external script corpus.** The four known forms are
real effect-script calls: `EFFECT_DETACH_KIND` 452 calls / 327 files,
`EFFECT_DETACH_KIND_WORK` 15 calls, `ENABLE_AREA` 14 calls, and `UNABLE_AREA` 12 calls. Their
wrapper arities are exact: two values after `agent` for each detach form and one for each area
toggle. The zero-call boundary above is therefore historical local-cache evidence, not a reason
to keep the feature opaque.

#### [x] C4a — Typed detach and area point controls (completed 2026-08-08; live runtime unverified)

All four commands are now typed as point `EffectCall`s rather than effect lifetimes. They keep
their script frame, never shorten a following effect, appear as purple event rows in the effect
panel, export with their measured wrapper shape, and support value-only source write-back. The
editor and verifier keep them out of spawn transforms, `LAST_EFFECT_SET_*` anchoring, and
`EFFECT_OFF_KIND` lifetime inference.

The live path captures all four hooked primitives, matches controls by command, exact captured
arguments, motion, frame window, and occurrence, and supports suppression/retiming/injection for
controls whose replacement arguments can be rebuilt. The external corpus is evidence for source
shape, not hardware evidence: no emulator, game, or UI automation was run.

#### [!] C4b — Work-slot detach runtime token boundary

`EFFECT_DETACH_KIND_WORK` reaches its primitive after the smash-script wrapper resolves the
authored `WorkModule` slot, so the plugin observes the runtime effect handle rather than the
source Work ID. Unchanged Work IDs can therefore be captured, suppressed, and replayed, and
frame/unknown-value edits can reuse that captured handle. An edited Work ID cannot be converted
without a verified runtime mapping; live preview leaves the original call running and reports
the limitation, while export and source write-back retain the edited authored token. This keeps
the remaining C4 boundary explicit instead of guessing at WorkModule semantics.

The earlier "measured after A3" line counted `SET_PLAY_INHIVIT` without reading its signature,
which is the trap two entries above this one warns about, applied to an entry's *own* evidence.
A count says a macro is used; only the signature says what it is.

**Inherited from B5 (2026-08-04):** `ENABLE_AREA(agent, kind: i32)` and
`UNABLE_AREA(agent, kind: i32)` were filed under B5 as detection volumes. They take one int
and no geometry — they turn an existing area on and off, which is this entry's subject, not
B5's. Both have **zero** corpus calls, so they are subject to the same round-trip-test bar
the detach half fails; do not schedule them on their own.

- **Trap:** end frames are currently derived from `EFFECT_OFF_KIND`. A detach ends a spawn's
  attachment without ending the spawn. Do not fold it into the same end-frame field.

## New script categories

### [x] D1 — `sound_` scripts (done 2026-08-06 — all five surfaces, live verified in game)

Blocked by: nothing, but it is the largest task here — treat it as its own project.

Not supported at all today. The project indexer recognises the `sound_` prefix
([acmd_src.rs:345](src/acmd_src.rs:345)) so it knows what the function is, but `script_body`
([acmd_src.rs:305](src/acmd_src.rs:305)) only ever loads `game_` and `effect_`. So `PLAY_SE`,
`PLAY_SE_NO_3D`, `PLAY_SEQUENCE`, `PLAY_STEP`, `PLAY_STEP_FLIPPABLE`, `PLAY_LANDING_SE`,
`PLAY_STATUS`, `PLAY_FLY_VOICE`, `PLAY_DOWN_SE`, `PLAY_SE_REMAIN`, `STOP_SE` are invisible.

Nothing is at risk right now — files holding them are never rewritten. That safety property
is what makes this task optional-until-scheduled rather than urgent.

**Measured 2026-08-05.** Sound is the largest unmodelled thing left in the corpus by a wide
margin — `PLAY_SE` alone has **429 calls**, against 386 for `ATTACK`. Counting the family:
`PLAY_SE` 429, `STOP_SE` 41, `PLAY_STEP_FLIPPABLE` 38, `PLAY_SEQUENCE` 28, `PLAY_SE_REMAIN` 21,
`PLAY_STATUS` 15, `PLAY_LANDING_SE` 12, `PLAY_DOWN_SE` 11, `SET_PLAY_INHIVIT` 10,
`PLAY_FLY_VOICE` 5 — about 610 calls across 301 of the 460 corpus files. If "largest task" was
the reason to defer it, "largest coverage gap" is the reason to take it.

- **Work order, in this order, each landing on its own:**
  1. **[x] D1a (done 2026-08-05)** — Load `sound_` in `script_body` and parse it to `Raw` only,
     with a corpus round-trip gate.
  2. **[x] D1b (done 2026-08-05)** — Merge project and mirror *per category*, which is what
     D1a named as step 2's own first move.
  3. **[x] D1c (done 2026-08-05)** — Type the `PLAY_SE` family and give sound a timeline lane.
  4. **[x] D1d (done 2026-08-05)** — Make a sound editable, and carry that edit through
     export and write-back.
     Note the generated plugin installs per category, so a `sound_` script it does not emit
     still plays vanilla — nothing is lost by sound being absent until here.
  5. **[x] D1e (done 2026-08-05)** — Let write-back *create* the category function a
     project does not have. This is the one surface D1b left open, and D1d turned it from an
     edge case into the ordinary one.
  6. **[x] D1f (done 2026-08-06)** — Plugin hooks, capture, and live edits for the sound
     primitives. See the sub-entry below.

**Work order revised 2026-08-05, and this is the correction Rule 5 asks for.** The old steps 4
and 5 were "plugin hooks, capture, then live" followed by "export + write-back", and *both
presume a sound can be edited*. Nothing in steps 1–3 makes one editable, and no step said it
would: the order jumped from a read-only timeline lane straight to previewing and persisting an
edit that cannot exist. The editing step is now step 4 and carries the two persistence surfaces
with it, because an edit that reaches neither the export nor the user's source is not an edit.

Live moved behind it for a second reason the DoD already states: it cannot be verified from this
machine at all. Shipping a live path before a durable one would mean a change you can hear once
and cannot save.
- **Trap:** the moment a *write* path lands, a file that was previously never rewritten becomes
  rewritable. The corpus round-trip is the gate for that, not a spot check.

#### [x] D1a — Load and re-emit `sound_` scripts (done 2026-08-05)

**Result:** `extract_function(source, prefix)` replaces the two copy-pasted extractors;
`parse_sound_script` / `emit_sound_body` read and write the body. Both are staged — the
corpus gate is their only caller, deliberately: the generated-source pane promises what an
export would write, and step 4 is where an export starts writing sound.

`every_sound_script_in_the_corpus_survives_a_round_trip` asserts three properties over all
**301** sound scripts, rather than the byte-equality the entry originally asked for. See the
trap about malformed sources for why. 295 are byte-exact; the other 6 differ only in the
indentation the dumper got wrong.

**Two pre-existing bugs the gate surfaced on its first run, both older than this entry and
neither about sound:**

- **A runtime branch lost the brace that closes it.** `parse_stmts` kept `if …{` as a one-line
  `Raw` and skipped every lone `}`. **35 of 236 `game_` scripts** exported a function with two
  more `{` than `}`, and an `if`/`else` had both arms promoted to unconditional — Kirby's Final
  Smash start issued `FT_START_CUTIN` twice. Fixed with `AcmdStmt::RawBlock`, which keeps the
  header, the walked body and the brace. `no_game_script_in_the_corpus_exports_an_unbalanced_function`
  is the standing gate; it asserts at least 30 scripts actually branch so it cannot pass
  vacuously.
- **A body with no `game_` function was read whole by the game parser.** Every line of an
  effect-only or sound-only move came back as `Raw`, and `emit_stmts` writes `Raw` into the
  generated `game_` function — so its effects would spawn twice, once from each script. The
  fallback now applies only to a body with no ACMD function header at all, which is the live
  capture and paste case it exists for.

**Deliberately unchanged:** a hitbox inside a branch is still walked as though the branch always
runs, which is what happened when branches were flattened. 18 corpus `game_` scripts place an
`ATTACK` this way and would lose it from the editor otherwise. The fix here was to the *brace*,
not to the condition; pinned by `a_hitbox_inside_a_branch_is_still_seen_by_the_frame_walk`.

#### [x] D1b — Merge the project and the mirror per category (done 2026-08-05)

D1a named this as a sound problem. It is not: it is an *every category* problem, and it was
already live for the most common mod shape there is.

`script_body` ([acmd_src.rs:312](src/acmd_src.rs:312)) concatenates whatever categories the
project defines and returns `Some`. `fetch_acmd` ([app.rs:1723](src/app.rs:1723)) treats
`Some` as the whole answer and drops the mirror fetch on the floor — including any fetch
already in flight. So a project that overrides only `game_attackairn`, which is what most
hitbox mods are, displays the move **with no effects at all**, and the editor cannot tell
that from a move that genuinely has none. The sound-only case is the same bug seen from a
different side.

- **The fix:** resolve each of the four categories independently — project's if it has one,
  mirror's otherwise — and concatenate the result.
- **The thing to get right is not the merge, it is the fetch.** The project path is
  deliberately inline and synchronous ("it is a local file read"). A partial override now
  needs the mirror too, so: use the disk cache when it is warm, spawn the existing worker
  when it is not, and carry the project's own parts through to the poll site so they still
  win on arrival. **A mirror fetch that fails must fall back to project-only** — offline
  must not be worse than today.
- **Test bar:** a project defining only `game_` keeps vanilla's effects; only `effect_` keeps
  vanilla's hitboxes; only `sound_` keeps both. A project category always beats the mirror's.

**Result:** `script_body` became `script_source`, returning `ProjectScript { body, covers }` —
the text and the categories it speaks for. `merge_project_over_mirror(project, covered, mirror)`
appends the mirror's functions for the categories `covered` does not name.

**The merge is asymmetric on purpose.** The project's text is carried verbatim and never
re-extracted per category, because a project function can be called anything and bound to a
script by attribute (`#[acmd_script(script = "game_attacks4")] fn my_custom_name`) — splitting
it by prefix would silently drop it. Only the mirror is safe to take apart that way.

**The fetch, which was the actual risk.** `needs_mirror()` consults `DISPLAYED_PREFIXES`
(`game_`, `effect_`) rather than all four. Requiring the mirror for a `sound_` nothing reads
yet would make an offline user with a complete `game_`+`effect_` project sit out the 20 s HTTP
timeout before seeing their own move. `sound_` joins that list at step 4, `expression_` at D2 —
the merge already fills them whenever a mirror body happens to be in hand, which is free.
Cold cache plus a partial override parks the project in `pending_project_script` and merges it
back in `merge_fetched_body`, extracted as a free function so the offline path is testable
without an app.

**Surface not completed, deliberately:** write-back. Editing a mirror-sourced effect in a
project with no `effect_` of its own now *reachable*, and `sync_script` bails with "the project
has no effect_attackairn to sync into". Refusing is the honest answer until something can
create the missing function; pinned by
`syncing_a_category_the_project_does_not_define_refuses_instead_of_guessing`.
**Closed by D1e (2026-08-05)**, which added the creating pre-pass rather than teaching this
function to guess — that test still passes, and is now the assertion that it never learned to.

`a_partial_project_override_keeps_every_category_it_does_not_define` runs all three shapes over
the corpus as the mirror, guarding on **132 moves with effects and 78 with hitboxes**. Its
sharpest assertion is the one that nearly was not written: two `game_` functions in a body
parse *fine*, first one wins, so a merge appending vanilla's copy underneath the project's
would go unnoticed until an export wrote both out. Four mutations run, all caught.

#### [x] D1c — Type the sound family and give sound a timeline lane (done 2026-08-05)

D1a reads a `sound_` function and writes it back unchanged; every line inside it is `Raw`, so
nothing knows a sound from a comment. This types the calls and puts them on screen.

**Measured 2026-08-05, before deciding the shape.** All **610** calls sit inside a `sound_`
function — not one is in a `game_` or `effect_` one — so this family cannot collide with the
`game_` parser the way `COL_PRI` collided with the pushbox one. Every call in the corpus is
written `macros::NAME(agent, Hash40::new("…")…)`; there are zero calls that pass anything but
a string literal, and zero written without the `macros::` prefix. Density is low: the median
sound script has **1** call and the busiest has 18, so one timeline row per call is affordable.

Arities, read off `macros.rs` rather than guessed — `PLAY_SE`, `PLAY_SE_NO_3D`,
`PLAY_SE_REMAIN`, `STOP_SE`, `PLAY_STEP`, `PLAY_SEQUENCE`, `PLAY_STATUS`, `PLAY_LANDING_SE`,
`PLAY_DOWN_SE` take one `Hash40`; `PLAY_STEP_FLIPPABLE` and `PLAY_FLY_VOICE` take two;
`SET_PLAY_INHIVIT` takes a `Hash40` and an `f32`.

- **Parse+IR:** `ExcuteStmt::Sound(SoundCall)`, resolved to frames by the *same* `eval_stmts`
  walk the hitboxes use, for the reason `to_hurtboxes` gives: `frame`/`wait` arithmetic and
  `for` unrolling decide when a sound fires just as much as when a hitbox opens.
- **Panel:** a sound band under the hurtbox band, and sound frames counted in the timeline's
  extent so a sound after the last hitbox is not cut off.
- **Named exception — Live and Export are out of scope, by this entry's own work order.**
  Steps 4 and 5 own them. Nothing here is editable, so nothing can be lost: the panel is a
  display of a script the export still writes back verbatim as `Raw`.
- **Trap:** match on `macros::NAME(` with the paren, never the bare name. `PLAY_SE` is a prefix
  of `PLAY_SE_NO_3D` and `PLAY_SE_REMAIN`, and `PLAY_STEP` of `PLAY_STEP_FLIPPABLE` — the same
  shape as the `ATTACK`/`ATTACK_ABS` collision this file already warns about.
- **Test bar:** the existing corpus round trip must stay byte-exact with the calls typed —
  that is the whole gate, because a typed call is now *regenerated* rather than copied. Plus a
  count of how many corpus sound calls are typed rather than left `Raw`, so a family member
  that stops being recognised fails instead of quietly falling back.

**Result:** `ExcuteStmt::Sound(SoundCall)` with `SOUND_FUNCS` naming each member's arity;
`AcmdScript::to_sound_events` resolves frames through `eval_stmts`. `AppState::sounds` feeds a
band under the hurtboxes, and `timeline_frame_extent` counts sound frames so the landing thud
75 frames after the last hitbox is reachable. The round trip stayed byte-exact — 295 of 301,
the same six mis-indented at source — which is what says a regenerated call is spelled the way
the game's own scripts spell it.

**The new oracle found a shape nobody had looked for, on its first run.** Fifteen corpus
`sound_` scripts write their last call *outside* every `is_excute` block — `kirby/WalkMiddle`
plays one footstep wrapped and the other bare. Those parsed as `Raw`, so the move showed one
footstep instead of two, and it had been that way since D1a. The round trip never noticed, and
could not: an unrecognised line round-trips *because* it is copied verbatim.

`AcmdStmt::Bare(Box<ExcuteStmt>)` is the fix. Deliberately not a one-statement `Excute`:
emitting that would add an `if macros::is_excute(agent) {` the source never wrote, which is a
behaviour change and not a formatting one. It made `eval_stmts` grow a second route into the
per-statement logic, so that logic moved into `eval_excute_stmt` and both call it — what a
command does cannot depend on whether its author wrapped it.

**Only sound calls are routed into `Bare`.** A bare `ATTACK` stays `Raw` exactly as before,
because nothing has measured whether one exists or what the timeline should do with it.

**The trap this shares with B4:** typing a line moves it out of `Raw`, and `Raw` is what the
hitbox rebuild keeps by name. A `Bare` the rebuild did not know about would be *deleted* on the
first hitbox drag — silencing a move as a side effect of moving a hitbox. Both rebuild passes
route it through the same collision filter a wrapped statement gets, so the answer cannot
depend on the wrapper; `a_bare_sound_survives_a_hitbox_rebuild` pins it.

Five mutations run, all caught — including the two that motivated their own assertions: a
`Bare` that is parsed but never walked (the corpus oracle counts statements, so only the
`kirby/WalkMiddle` event test sees it) and a needle matched without its paren, which reads
`PLAY_STEP_FLIPPABLE` through `PLAY_STEP`'s one-hash layout.

**Still out of scope, per this entry's own work order:** live playback and export plus
write-back. Sounds are displayed and nothing else — `sound_` is still not written by
any export, so nothing can be lost.

#### [x] D1d — Make a sound editable, and persist it (done 2026-08-05)

D1c put sounds on screen and stopped there. This makes one editable and carries the edit to the
two places an edit has to reach: the generated plugin, and the user's own source.

**Scope: which sound a call plays, and nothing else.** Changing `se_kirby_swing_l` to
`se_common_swing_m` is one argument in one call, which is exactly what the write-back path is
built to do. Retiming, adding and deleting are deliberately excluded — a sound's frame is the
block it sits in rather than an argument, which is the same reason `rewrite_hurtboxes` reports a
retime instead of performing one, and an added call has no site to write to.

- **Parse+IR:** `SoundEvent.site`, on its own counter. **Not shared with `next_site` or
  `next_mod_site`** — sharing one is the trap this file has already paid for, where a later
  family's edits silently retarget and every per-family test still passes.
- **Panel:** the sound rows become editable fields. `sounds_pristine` beside `sounds`, because
  every write-back path here diffs against the pristine parse rather than tracking dirtiness.
- **Export:** `sound_` joins `DISPLAYED_PREFIXES` and `build_mod_project_full` emits the
  function. The D1a round trip already measured what this writes: 295 of 301 byte-exact, the
  other six differing only in an indentation their source got wrong.
- **Write-back:** `sound_sites` + `rewrite_sounds`, matching the `rewrite_hurtboxes` shape.
- **Named exception — Live is out of scope**, and is now step 5. It needs hardware this machine
  does not have; see the DoD's own wording on not implying the live surface was exercised.
- **Trap:** `count_hurt_stmts` steps the site cursor over a zero-iteration `for`, and the sound
  counter needs the same treatment — but it must also count `AcmdStmt::Bare`, which the hurtbox
  one never had to. Fifteen corpus scripts put a sound outside every `is_excute` block, so a
  counter that ignores `Bare` mis-numbers every site after one.

  **That last sentence was wrong when this entry was written, and the mutation pass caught it.**
  `count_sound_stmts` runs only for a loop body, so ignoring `Bare` there matters only for a bare
  sound inside a `for` that runs *zero* times — which no corpus script contains. The place `Bare`
  is genuinely load-bearing is `sound_stmt_mut`, which resolves a site for editing: drop it there
  and `kirby/WalkMiddle`'s second footstep is uneditable, and a wrapped call after a bare one is
  edited by writing to the wrong line. Both are pinned now, by
  `a_loop_that_never_runs_still_advances_the_site_cursor` and by the edit half of
  `a_sound_written_outside_an_excute_block_still_fires`.
- **Test bar:** a corpus oracle asserting that every event's site indexes into the *textual*
  scan and lands on a call of the same macro. That is the assertion that catches a retargeted
  edit, which no round trip can see: a mis-sited edit writes a perfectly well-formed script.

**Result:** `SoundEvent.site` on `WalkAccum::next_sound_site`, its own counter;
`AcmdScript::sound_stmt_mut` resolves one for editing. A Sounds section under the hurtboxes edits
the name; `resolve_sound_state` decides what a reopened move shows. `rewrite_sounds` +
`sound_sites` write it back, `build_mod_project_full` takes a fourth list and emits `sound_`
functions, `verify_sound_move` reads each one back before it reaches disk, and `sound_` joined
`DISPLAYED_PREFIXES`. 361 green.

**The site oracle found the corpus cannot test loops at all.** A guard asserting some corpus
sound script loops a call failed on its first run: **zero of 301 contain a `for`**. So the loop
rewind — the whole reason a site is a source ordinal rather than an execution counter — has no
vanilla script to prove it. `a_looped_sound_reports_the_same_site_every_time_round` composes the
`for _ in 0..3 {` header real `effect_` scripts write with `kirby/TurnDash`'s own calls, which is
a join of two corpus-verified shapes rather than an invention; the entry says so in its own
doc comment so a later reader does not mistake it for lifted evidence.

**Ten mutations run, and three of them exposed missing assertions rather than confirming
existing ones**, which is the part worth carrying forward:
- Dropping `sound_sites`' arity filter passed everything. A `macros::PLAY_SE(agent)` typed by
  hand parses to `Raw` but a name-only scan counts it, so every later site resolves one line
  early — the rename lands on the broken call. No corpus script is malformed, so only a
  deliberate fixture reaches it: `a_malformed_sound_call_does_not_take_a_site`.
- `sound_stmt_mut` ignoring `Bare` passed everything, per the correction above.
- Folding the saved edit into `sounds_pristine` passed everything, and it is the worst of the
  three: write-back would diff an edit against itself, find nothing, and report zero changes with
  no error anywhere. Untestable as written, because it lived in a method needing a whole `App`
  — so the decision moved into the free function `resolve_sound_state`, which returns the
  baseline *and* the shown list so a caller cannot use one for both.

**Named exceptions.** Live is step 5 and was not exercised: this machine has no Switch and no
emulator, and the plugin was not touched. Retiming, adding and deleting a sound are out of scope
by the entry's own scope line, and the panel offers no widget for them rather than offering one
whose change comes back reported as skipped.

#### [x] D1e — Create the category function the project does not have (done 2026-08-05)

**This is not a sound task, and it only lives under D1 because that is where the debt was
incurred.** It is surface 5 for every category at once.

D1b made a mirror-sourced script *editable* in a project that does not define it, and stopped
there: `sync_script` ([acmd_src.rs:920](src/acmd_src.rs:920)) bails with "the project has no
`effect_attackairn` to sync into". Refusing was the honest answer while the case was rare.
D1d made it the common one — `sound_` joined `DISPLAYED_PREFIXES`, so **every** project without
a `sound_` function of its own now shows an editable sound section whose edits cannot be saved.
A hitbox mod is exactly that project. The user renames a sound, the panel accepts it, the export
carries it, and the sync says no.

- **The fix is not an emitter.** Do *not* regenerate the function from the IR: the effect
  emitter deletes lines it could not type (C5, C6), so creating an `effect_` function that way
  would write a lossy copy of vanilla into the user's project and call it theirs. Take the
  **mirror's verbatim function text**, run the ordinary value rewrite over it, and write *that*.
  Creation then obeys the same rule every other write obeys: argument values change, nothing
  else does, and the result differs from vanilla only where the user edited it.
- **The registration is the part that can silently do nothing.** A function smashline never
  installs is dead text, and a dead function is worse than a refusal because it looks like it
  worked. Derive the registration from a sibling script of the same fighter: copy its
  `#[acmd_script(...)]` attribute with the script name and category substituted, or add a
  parallel `agent.acmd("…", …, …)` line beside its own. A sibling that carries **neither** is
  registered by convention or not at all — mirror it exactly and say so in the report, because
  guessing a registration the project's own style does not use is the failure above.
- **Refuse when there is no sibling at all.** No sibling means no file to write into, no fighter
  attribution, and no registration shape to copy. That case keeps today's error.
- **The index is stale the moment a file grows a function.** Every `ScriptSite` span after the
  insertion point shifts. Rebuild the index rather than patching spans — creation is a rare
  user action and a whole rescan is correct by construction, where offset arithmetic across two
  insertions in one file is the kind of thing that works until it does not.

**Scope:** creating the function and registering it. Not creating a *fighter* — a project that
says nothing about the character is still out of scope, and still an error.

- **Test bar:** creating into a project registered by `agent.acmd` produces a file that names the
  new script in the install block; creating into an attribute-registered project produces the
  attribute; a created function re-indexes to exactly the script name that was asked for; the
  created text differs from the mirror's only at the edited argument; and a project with no
  sibling is still refused with a message naming what is missing.
- **The trap this shares with D1d:** a created function that is registered under the wrong script
  name parses, compiles, and round-trips. Assert the *re-indexed* name, not the file's contents.

**Result:** `create_script` ([acmd_src.rs:711](src/acmd_src.rs:711)) writes the function and its
registration; `VisionaryApp::create_missing_scripts` runs it as a pre-pass before the value syncs.

**Creation and the value write stayed separate, and that is the design decision here.** The
created function is vanilla *verbatim* — not the edit — and the ordinary sync writes the values
into it a moment later, through the same code every other edit goes through. Threading a "create
from this text" seed down into `sync_script` was the first shape tried and it is worse: three of
the five sync passes share the `game_` function, so the first would create it and the second and
third would then be resolving spans against an index a write had already invalidated. Splitting
them means creation never learns what an edit is, the value write never learns the function is
new, and `syncing_a_category_the_project_does_not_define_refuses_instead_of_guessing` still
passes — a sync never gained the power to invent a destination.

**Registration is the half that can silently do nothing**, and it drove every refusal here.
A function smashline never installs is dead text that compiles, and that is worse than an error
because it looks like it worked. Three shapes:
- `agent.acmd("…", fn, …)` — copied with both names respelled from the arg spans, so a script
  name that is a prefix of the function name cannot be substituted into the wrong slot.
- `#[acmd_script(...)]` — copied with `script` respelled and `category` *derived*. There is no
  copy of that macro on this machine, so `ACMD_SOUND` is a name read off the project rather than
  looked up: it is only emitted when the sibling's own token is exactly the `ACMD_<CATEGORY>` its
  own prefix implies. A project spelling it `Acmd::Game` is refused, and writes nothing.
- Neither — created under the conventional name and *said out loud* in the status line.

**A gap found on the way, not part of the entry.** The sound sync pass sat inside the `game_`
guard, so a project defining only a `sound_` function had its sound edits silently skipped: the
pass that would have written them was gated on a function that project has no reason to define.
It is now its own pass, guarded on its own script.

**Six mutations run, three of which exposed missing assertions:**
- Sorting the two insertions ascending instead of descending — caught, twice over.
- Deriving the category unconditionally — caught.
- Dropping the "already exists" guard **survived**. Creating a duplicate function is not a wasted
  write, it is two definitions with one name and a project that stops compiling.
- Dropping the same-move anchor preference **survived**. Both files produce a working script, so
  nothing downstream can tell them apart — which is exactly why the choice needed its own test.
  The symptom is a sound for the aerial written into the specials file, for the user to undo.
- Reading *any* `#[...]` above a sibling as its registration **survived**, and it is the worst of
  the three: an `#[allow]` gets copied above the new function, the real `agent.acmd` line is never
  looked for, and the result compiles, installs nothing, and plays vanilla. Exactly the failure
  the whole registration path exists to prevent, and it passed every test in the file.

**Named exceptions.** Live is untouched and unaffected — this is a disk surface. Creating a
*fighter* is still out of scope and still an error: a project that mentions the character nowhere
has no file to write into and nothing to attribute a new function to.

#### [x] D1f — Hook, capture and preview the sound family (done 2026-08-06)

**Taken now because the "needs hardware" deferral was measured and found false.** D1d and D1c
both say this machine has no emulator. Eden and Ryujinx are installed with SSBU under each, both
carry a plugins directory holding this plugin, and the last session ran 2026-08-05. The deferral
was never checked. The DoD now carries the one-line command that checks it.

**The deployed build was `2026-08-03f`, 66 commits stale**, so the live surface of A2, A3, B1,
B3, B4, B5, B5b, C1, C2 and C3 had never run either. This step was written *before* the boot so
one session verifies sound alongside all ten of those, which is the whole reason it was
scheduled ahead of the rest.

**Result.** `hitbox_viewer/sound_hooks.rs` hooks all twelve members, records every call into the
existing capture stream, and applies rename and suppress rules.
`VisionaryApp::sound_script_from_captures` reads them back, `sound_rules_for` builds the rules,
and `CAT_SOUND` = 8 carries them. 422 unit + 6 integration green, clippy 0, plugin builds at
2334720 bytes with `build=2026-08-06a-sound-hooks`.

- **No member is missing a wrapper, checked both ways.** All twelve are declared in `sv_animcmd`
  *and* in `smash-script`'s `macros.rs`, so unlike A1's `AREA_WIND_2ND` there is no primitive the
  plugin can hook but an export cannot write. The arities agree with what D1c read.
- **One category for twelve members, which is the opposite of the `CAT_ATK_POWER` decision and
  is not an inconsistency.** There, slot 1 meant a different thing in each member, so a
  misapplied rule wrote damage into a shield multiplier. Here every member declares a `Hash40` in
  slot 0 and nothing else is ever written, so the worst a misapplied rule can do is put a sound
  where a sound goes. Twelve categories to keep in step across the wire is a worse trade than one
  name comparison, so the macro name travels on `HitboxRuleWire::func` and the plugin requires it
  to agree. A rule with no `func` matches any member — which is what an older editor sends, read
  in the only safe direction: too broad rather than silently dead.
- **`SET_PLAY_INHIVIT`'s trailing argument is never writable**, and that is what `hash_slots`
  exists for. It is a `ToF32` duration, not a sound; bounding the write by the vector's length
  instead of the member's declared hash count would put a hash40 there and suppress the sound for
  an absurd number of frames.
- **An unnameable sound is dropped, not kept as a number** — the rule the bone path states, and a
  live case rather than a hypothetical: `ParamLabels.csv` names about ten thousand `se_*` labels
  and is missing real ones, `se_common_step_left_m` among them. A half-nameable
  `PLAY_STEP_FLIPPABLE` is dropped whole, because a one-argument call of it is a different
  signature that does not compile.
- **Scope is D1d's scope: which sound a call plays.** Retiming, adding and deleting stay out, so
  a rule only ever rewrites hash arguments or suppresses the call. Write-back and export are
  unchanged — D1d and D1e already own them.

**The plugin's own tests were deleted rather than written.** That crate is not a workspace
member, so `cargo test` never builds it and a `#[test]` there is a comment that looks like a
gate — the same shape as D1e's unregistered function. Everything checkable off the source is
checked from `game_link.rs` instead: the plugin's `SOUND_FUNCS` is compared row for row against
the editor's, and every `sound_hook!` arity literal against the declared one. Both tables are
deliberately spelled the same way so the comparison is literal.

**Ten mutations, four of which survived the first round of tests** — and all four are the same
failure, which is the one this family is most exposed to:

- Plugin table drifts, hook reads the wrong arity, plugin `CAT_SOUND` drifts, unnameable hash
  kept, half-pair accepted, duration re-spelled as a float, window covering only the first
  iteration — all caught.
- **The rule keyed on the edited sound instead of the pristine one.** Survived the whole suite.
  It produces a rule that is well-formed, serialises, sends, deserialises, and never matches.
- **The macro name dropped from the rule.** Survived. A `PLAY_SE` rule then also applies to a
  `PLAY_SE_REMAIN` naming the same sound in the same frame, cleanly, because slot 0 is a hash in
  both.
- **The capture adoption never setting a baseline.** Survived. Fails in both directions: no
  baseline reads as "every call is edited" and sends a rule for each; a baseline taken from the
  edited list reads as "nothing is edited" and sends none.
- **Adoption overwriting a fetched sound script.** Survived. A capture only sees the calls that
  actually ran, so it is a lossy trade that also discards every verbatim line the export carries.

All four lived inside `push_sound_rules(&mut self)` and the capture-adoption block, which no test
could reach. They are now `sound_rules_for` and `adopt_captured_sounds`, free functions taking
what they decide over — the `record_effect_script_notes` move from E3, made for the same reason
and after the same four survivors. **Every one of them is a live edit that silently does
nothing**, which is exactly what B5b cost two tasks to find, and none of them is visible to any
oracle in this project: the export is unaffected, the write-back is unaffected, and the panel
looks right.

**Verified in game 2026-08-06**, after D1g fixed the one branch that stopped it working.
Renaming Kirby's up-tilt sound to the electric hit plays the new sound live. All five surfaces
are now coherent for the `PLAY_SE` family, which closes D1.

The staged confirmation, `build=2026-08-06c-sound-firstline` on Eden:

```
ACMD SOUND hooks installed (12 macros, capture + rules)
SND first captured sound: PLAY_STEP_FLIPPABLE
```

Confirmed by that: the twelve hooks install on a real boot, and a hook fires. That it fired as
`PLAY_STEP_FLIPPABLE` is worth more than a bare "a sound was seen" — it is one of only two
members taking a *pair* of hashes and its name is a prefix of `PLAY_STEP`, so the call resolved
to the right macro at the right arity rather than through its shorter sibling's layout.

Capture reaching the editor and the rename applying live are both confirmed since. **Suppress is
still unexercised** — the code path is shared with the rename and differs only in returning early
instead of rewriting, but nothing has pressed it.

**Two diagnostic bugs cost two boots, and both are the same shape.** The install banner first
went out through `skyline::println!`, which reaches the Skyline log, while `sd:/slight/diag.txt`
is written by `diag::note` — so the check written for it ("grep diag.txt for the banner")
returned zero and proved nothing. Then `write_capture_diag("installed")`, being the first
statement of `install()`, snapshot a `sound_hooks` flag that a call twenty lines below it sets,
so it read `false` on every boot regardless; and its `recorded=` counter only refreshes at
install and at drain, so `recorded=0` meant "the editor has not pulled yet" rather than "nothing
fired". A working boot read as a broken one, twice.

**The lesson is [[verification-findings-need-a-channel]] pointed at the boot itself: a signal
that costs nothing to emit is worthless if it lands somewhere nobody reads, and a flag is worse
than no flag when it is sampled before the thing it reports.** Check *when* a diagnostic file is
written before reading a number out of it. The one-shot `SND first captured sound:` line is the
shape that worked: it names what happened, it does not depend on the editor being connected, and
it is one buffered write per boot.

#### [x] D1g — Live sound edits matched on the type tag and never fired (done 2026-08-06)

D1f built the rule path and could not verify it. It did not work, and the reason was one branch:
`sound_action_for_call` read the rule key with `Some(LuaArg::Hash(h)) => *h, _ => return false`.
The game passes a sound as `L2CValueType::Int`. Kirby's up tilt hands over `0x10556b83cc`, which
is `hash40("se_kirby_swing_l")` exactly — **the right value, rejected on its type**.

**Live capture worked the whole time**, because the editor's `as_hash` already accepted `Int`.
So sounds appeared in the panel, could be edited, produced a well-formed rule that reached the
plugin — and matched nothing. Half a working feature is a worse signal than none.

- **Fixed** by `hash_arg`, which takes `Hash` or `Int` and masks to 40 bits, and by writing the
  override back **under the tag that arrived** rather than always as `Hash` — reproducing the
  call's shape as well as its value.
- **Pinned** by `game_link::a_sound_hash_is_read_whether_it_arrives_as_a_hash_or_an_int`, which
  checks both wire variants *and* greps the plugin source for the `Int` arm, since that crate is
  outside the workspace. Mutation-checked: deleting the arm fails it.

**Four game restarts went into the diagnostic rather than the bug, and that is the lesson worth
keeping.** Three separate reasons the log said nothing:
1. The install banner went out through `skyline::println!`, which is not the file being grepped.
2. The counters in `effect_viewer_capture.txt` are written at install and drain only, so
   `recorded=0` meant "nothing has drained", not "nothing fired".
3. The report budget was per boot, so twelve "no rules loaded" lines during ordinary play
   exhausted it 2400 lines before the rule under test arrived.

Only the third round — reporting *every* early return, with a budget reset per rule set — named
the branch. **A branch that returns silently is invisible in exactly the case you are debugging
it**, and the cost of adding a line to each one is nothing next to a restart.

### [x] D2 — `expression_` scripts (completed 2026-08-08; live runtime unverified)

Blocked by: D1 (reuse its loading and round-trip machinery wholesale).

**Measured 2026-08-07 before scheduling.** The local cache has 335 `expression_` function
bodies. The first emittable slice is `RUMBLE_HIT` (65 calls), `QUAKE` (51 calls), and
`FT_ATTACK_ABS_CAMERA_QUAKE` (2 calls), all confined to expression scripts. The screen-fill
calls named in the original entry are not expression calls: `FILL_SCREEN_MODEL_COLOR` and
`CANCEL_FILL_SCREEN` each occur twice in `effect_` functions and belong to the effect-family
residue work instead. They are removed from this scope rather than claimed by the wrong lane.

Implement the three measured expression macros through the same staged parse/IR, panel, live,
export, and source-write-back gate. Unknown expression lines stay verbatim until a later
measured slice; a parser that drops raw rumble or slope/module lines is not full expression
coverage.

**Result 2026-08-08.** The measured slice now has one shared IR/event/site walk, an editable
`expression_` panel and source checkout, timeline coverage, capture adoption, sparse live rules,
portable project persistence, generated Skyline source, export read-back verification, and
argument-only source write-back. `RUMBLE_HIT`, `QUAKE`, and `FT_ATTACK_ABS_CAMERA_QUAKE` retain
their source tokens; structural changes are reported instead of invented. The local corpus gate
still measures 335 expression bodies and exactly 65, 51, and 2 calls respectively, all typed and
round-tripped. The plugin's category-10 wire lane and three hooks build successfully; live game
behaviour remains unverified because no emulator or UI automation was run.

## Gameplay

### [ ] E1 — Movement and kinetics (the measured `REVERSE_LR` slice is complete)

`sv_kinetic_energy` and status-module kinetic calls remain preserved verbatim today.
`SET_SPEED`, `ADD_SPEED_NO_LIMIT`, `CORRECT`, and `SET_SPEED_EX` now have measured, value-editable slices
below. High mod value — this is how a move's momentum and correction are authored — and the editor
already knows the frame each call lands on.
`REVERSE_LR` is handled in the separate measured slice below.

**Measured 2026-08-08, and this is the Rule 5 correction:** the local cache has no calls for
`sv_kinetic_energy`, `ADD_SPEED_NO_LIMIT`, or `CORRECT`; the single `KineticModule`
hit in the whole cache is `KineticModule::change_kinetic` in Kirby `EscapeAir`, a raw status-module
call rather than an ACMD macro. A read-only public dumped-script corpus has 144 textual
`SET_SPEED_EX` calls: 122 match the vendored three-argument wrapper shape, while 22 are malformed
dump artifacts (17 short and 5 with one extra argument). The same corpus has 3 exact
`ADD_SPEED_NO_LIMIT(agent, x, y)` calls and 31 exact `CORRECT(agent, kind)` calls, both covered
by vendored `smash-script` wrappers. The same corpus has **2 exact textual
`macros::SET_SPEED(agent, x, y)` calls**. `SET_SPEED` has a linked primitive but no safe Rust macro
wrapper in the vendored crate, so the generated source uses a local Lua-stack helper over that
primitive while editable source keeps the original macro spelling. Kinetics outside these
verified ACMD shapes remain outside this editor's current input boundary.

`REVERSE_LR` is the local-cache exception that started this slice: **7 real calls**, all
`macros::REVERSE_LR(agent)`, all Kirby (`ItemLightThrowB`, `ItemLightThrowB4`,
`ItemLightThrowAirB`, `ItemLightThrowAirB4`, `ItemHeavyThrowB`, `ItemHeavyThrowB4`, `EscapeF`).
It takes no arguments, so "editing" it means placing and removing it on a frame, not tuning a
value — which is a much smaller task than the entry's framing implies and shares nothing with
the speed macros.

- **Counting trap, hit while measuring this.** A substring grep says 9, not 7. The extra two are
  `FIGHTER_DOLLY_STATUS_SPECIAL_HI_WORK_FLAG_REVERSE_LR` in `WorkModule::on_flag` — a flag name
  that ends in the macro name. Word-boundary the pattern and read the call site; see the
  `ACMD family prefix and const collisions` note.
- **Scope decision.** `REVERSE_LR`, the verified `SET_SPEED` and three-argument `SET_SPEED_EX`
  shapes, and the exact `ADD_SPEED_NO_LIMIT`/`CORRECT` wrapper shapes are handled as their own
  slices below. The remaining zero-count speed/kinetic names, malformed `SET_SPEED_EX` shapes,
  and status-module sources remain parked, so the full E1 entry remains open.
- **Trap:** these change where the fighter *is*, so previewing them means moving the model,
  not drawing a box. Scope the first pass to editing values with no viewport preview, and say
  so in the entry when you take it.

#### [x] E1a — `REVERSE_LR` point events (completed 2026-08-08; live runtime unverified)

The measured seven-call family now has one typed ACMD statement/event walk with source ordinals,
one-based frame conversion, timeline markers, a Movement panel for add/remove/move, capture
adoption, project/edit-log persistence, generated ACMD export, and flat-source write-back. The
live surface uses category 11: an observed call can be suppressed, and an edited point can be
injected through the exact `sv_animcmd::REVERSE_LR` hook with an explicit zero-argument command.
Branches and loops are retained but structural source placement changes in them are reported as
unsupported rather than guessed into an execution arm.

The desktop viewport does not simulate the fighter's facing change in this first slice, and no
emulator, game, or UI automation was run. The plugin builds and the offline contract tests cover
parse/IR, timeline/capture conversion, panel model edits, wire parity, export read-back, and
source write-back; live in-game behavior remains unverified.

#### [x] E1b — verified `SET_SPEED_EX` velocity points (completed 2026-08-08; live runtime unverified)

The three-argument wrapper shape — `SET_SPEED_EX(agent, speed_x, speed_y, kinetic_kind)` — now
has a typed statement/event walk, one-based timeline points, an editable Movement panel, capture
reconstruction, portable project persistence, category-13 wire rules, the matching Skyline hook,
generated ACMD export, and value-only write-back of the two velocity arguments. The kinetic-kind
token remains source-owned, so named constants are carried exactly rather than guessed into a
portable numeric label.

Malformed dump forms remain raw: the public corpus's 17 short calls and 5 over-arity calls are
not padded or rewritten. Source syncing also refuses loop-unrolled or structurally changed sites.
For live edits, a numeric captured kinetic kind is used as the rule key; an unkeyed call is only
accepted when its frame has one speed point, and otherwise the UI reports the evidence gap.
Offline tests cover parse/IR, panel/timeline/capture conversion, wire/plugin agreement, export
read-back, and source write-back. No emulator, game, or UI automation was run, so live in-game
behavior remains unverified.

#### [x] E1c — `ADD_SPEED_NO_LIMIT` and `CORRECT` point events (completed 2026-08-08; live runtime unverified)

The exact vendored-wrapper shapes — `ADD_SPEED_NO_LIMIT(agent, speed_x, speed_y)` and
`CORRECT(agent, kind)` — now have typed statement/event walks, independent source ordinals,
Movement-panel rows, timeline lanes, capture reconstruction, project/edit-log persistence,
category-14/15 live rules with matching Skyline hooks, generated ACMD export, and value-only
source write-back. `ADD_SPEED_NO_LIMIT` edits use their captured frame, refusing same-frame
ambiguity because the primitive has no identifying argument. `CORRECT` preserves named source
tokens through parse/export/write-back; live replacement is limited to numeric captured keys and
numeric replacement kinds, with named cases reported as unrepresentable rather than guessed.

Malformed arities remain raw. Offline regression coverage includes parse/IR, malformed preservation,
panel/timeline/capture conversion, live-rule keying, wire/plugin parity, export read-back, source
write-back, and the release plugin build. No emulator, game, or UI automation was run, so live
in-game behavior remains unverified.

#### [x] E1d — verified direct `SET_SPEED` velocity points (completed 2026-08-08; live runtime unverified)

The exact three-argument source shape — `SET_SPEED(agent, speed_x, speed_y)` — now has a typed
statement/event walk, one-based timeline points, an editable Movement panel, capture
reconstruction, portable project persistence, category-16 live rules with a matching Skyline
hook, generated ACMD export, and value-only write-back of the two velocity arguments. Because the
vendored Rust macro layer exposes no safe `SET_SPEED` wrapper, generated source calls a local
helper that prepares the Lua stack and invokes the linked primitive; editable source retains the
original `macros::SET_SPEED` spelling. Source syncing also recognizes the generated
`visionary_set_speed` helper calls, while excluding the helper definition itself from the event
ordinal.

The two measured external calls and all exact local shapes round-trip; malformed arities remain
raw. Live replacement is frame-only and refuses same-frame pairs because the command has no
identifying argument. Offline regression coverage includes parse/IR, malformed preservation,
panel/timeline/capture conversion, live-rule keying, wire/plugin parity, export read-back, source
write-back, and the release plugin build. No emulator, game, or UI automation was run, so live
in-game behavior remains unverified.

#### [x] E1e — measured `FT_CATCH_STOP` point events (completed 2026-08-08; live runtime unverified)

The exact two-argument wrapper shape — `FT_CATCH_STOP(agent, arg1, arg2)` — now has a typed
statement/event walk, one-based timeline points, editable Movement-panel arguments,
capture-reconstruction support, category-17 live rules with a matching Skyline hook, generated
ACMD export, and numeric value-only source write-back. The source corpus gate measured 40 calls in
38 files, all with two numeric arguments; the local cache has no calls, so this measurement comes
from the read-only public dump used for the coverage decision.

Structural placement and add/remove changes remain export/source operations, and malformed or
non-numeric shapes remain raw. Live in-game behavior is unverified because no emulator, game, or UI
automation was run. The parent E1 entry remains open for `CLR_SPEED`, `SET_AIR`, status-module
kinetics, and other unmeasured or unsupported sources.

### [x] E2 — Model `FT_MOTION_RATE` (done 2026-08-06 — live surface unverified in game)

`FT_MOTION_RATE`, `FT_MOTION_RATE_RANGE`, `FT_DESIRED_RATE` are preserved verbatim, and their
presence deliberately **disables the export timing checks** (`check_script_shape`, via
`has_unmodelled_flow`).

**Rule 5 correction, measured 2026-08-06 — two of the three named macros do not exist here, and
the stated payoff was wrong.** Counted over the 461-file cache with the real parser, not a grep:

| | count |
|---|---|
| `FT_MOTION_RATE` calls | 17, in 10 functions |
| `FT_MOTION_RATE_RANGE` calls | **0** |
| `FT_DESIRED_RATE` calls | **0** |
| functions parsed | 432 |
| functions gated by *any* unmodelled line | 107 |
| functions gated **only** by a rate call | **9** |

So modelling rate re-enables the timing checks on **9 of 432 functions (2%)**, not "a large slice
of the corpus" as this entry claimed. **That sentence was written from the macro's importance, not
from a count** — the same mistake E1 was corrected for, and the reason the working agreement says
to count a macro before scheduling the entry built on it. Scope this to `FT_MOTION_RATE` alone;
the other two are unschedulable until a fighter that uses them is in the corpus.

**The entry is still worth doing, for the other reason it lists.** 7 of the 17 calls are
`FT_MOTION_RATE(agent, 1.0)`, a no-op restore, so there are 10 live rate windows — and they sit on
`attack_11`, `attack_hi4`, `attack_lw4`, `attack_air_n`, `attack_air_hi`, `special_n_start`,
`special_lw`, `cliff_escape`. Those are headline moves, the timeline currently shows the wrong
frame numbers for all of them, and the rate value **cannot be edited at all** today. That is the
justification; the verifier is a side effect of it.

## The direction, measured

**`FT_MOTION_RATE(agent, r)` advances the motion by `1/r` motion frames per game frame.**
Therefore `game_frames = motion_frames × r`. **This is reading B** — the reading
`FT_MOTION_RATE_RANGE`'s own arithmetic implied, and *not* the community reading the earlier
draft of this entry led with.

Measured live on Eden, `build=2026-08-06g-rate-multi`, three moves and two distinct arguments:

| move | script arg | reading A predicts | reading B predicts | **measured delta** | samples |
|---|---|---|---|---|---|
| `special_n_start` | 0.5 | 0.5 | 2.0 | **2.0000** | 9 consecutive |
| `attack_11` | 0.5 | 0.5 | 2.0 | **2.0000** | 1 (short window) |
| `attack_hi4` | 0.6 | 0.6 | 1.6667 | **1.6667** | 3 consecutive |
| `attack_lw4` | 0.25 | 0.25 | 4.0 | **4.0000** | 1 (short window) |

Cross-check: `special_n_start` holds the rate from motion frame 0 to 18 and crossed it in 9 game
frames. `18 × 0.5 = 9`. The frames either side of every window are exactly `delta=1.0`, which is
what says the probe samples once per game frame rather than missing ticks.

**A rate below 1 makes a move play FASTER in game-frame terms.** It compresses motion frames into
fewer game frames — it is skipping windup, not slowing it. Counter-intuitive, load-bearing, and
the opposite of what "rate" reads like; say it in the panel when this is built.

## The observable, which is not the one the first probe assumed

- **`MotionModule::rate` is useless here** — it reads `1.0000` throughout every window above.
- **`MotionModule::whole_rate` IS the multiplier**, and equals `1/arg` exactly: `2.0000` while
  `special_n_start` holds 0.5, `1.6667` while `attack_hi4` holds 0.6, back to `1.0000` on the
  frame the script restores it. **A live capture can read the effective rate straight off it**,
  with no need to parse the macro out of the script.

The animation sequencer's `at_end_frame` (`end <= frame + rate`) uses `motion_rate()`, which
prefers `whole_rate` when it is not 1.0 — so that code was right all along, and the earlier note
in this entry claiming it supported reading A was wrong about *which accessor* it consulted.
Withdrawn.

## Work order, now that the direction is known

- **Timeline:** rate scales the frame advance for everything after it, so the timeline must show
  real game frames once one is in play. `game = motion × r` over each span.
- **All four categories.** A `game_` script's rate calls change the pacing of that move's
  `effect_` and `sound_` scripts too, so whatever the timeline does with rate applies across all
  of them, not just the one the call is written in.
- **Done when:** the timing checks run on rate-carrying scripts and are *correct* on the corpus.
  Re-enabling them while wrong is strictly worse than today.
- **Trap:** branches (`if(WorkModule::is_flag(…)){`) are excluded from timing checks for a
  separate reason and are **not** in scope here. Leave that exclusion alone.
- **Trap, from the measurement:** a move can *restart* without the motion hash changing —
  `attack_11` jabbed twice and the frame went 5 → 0, a negative delta. Anything walking frames
  forward has to treat a backwards step as a restart rather than as a rate.
- **~~Delete the probe~~** — done 2026-08-06, along with its call in the animation-sequencer
  facade. The measurement it existed for is recorded above.

## Done 2026-08-06 — all five surfaces

`AcmdStmt::MotionRate(f32)` is parsed, exported, editable, written back, and applied live.
Adding the variant made the compiler enumerate the five matches that had to handle it, which is
the right tool for a change of this shape and found two sites review would not have.

- **The direction was independently confirmed from the macro's own source**, after being measured
  live. `smash-script`'s `FT_DESIRED_RATE(agent, motion_frames, game_frames)` passes
  `game_frames / motion_frames` as the rate, so `game_frames = motion_frames × rate` is the
  macro's own arithmetic. Two unrelated methods agreeing is the strongest form this entry could
  have reached, and it settles the reading for good.
- **One hook covers all three macros.** All of `FT_MOTION_RATE`, `FT_MOTION_RATE_RANGE` and
  `FT_DESIRED_RATE` compile to the same `sv_animcmd::FT_MOTION_RATE` with one `f32` on the stack;
  the longer forms just divide first. So the live surface needed no family table and cannot drift
  from the editor's the way the sound one can.
- **`eval_stmts` is deliberately untouched.** It resolves the frames a script *names*, which are
  motion frames, and every hitbox range is keyed to them. Rate is a separate motion → game
  mapping (`rate_spans` / `game_frame`); applying it in the walk would move every hitbox to a
  frame its own source never mentions.
- **The verifier claim, checked rather than asserted.** The timing checks now run on all 10
  rate-carrying corpus functions and fire exactly one warning, on `kirby/SpecialLw` — and that
  warning is **true**: the script really does `frame(14.0)` and then `frame(2.0)`, so the
  `AttackModule::clear_all` runs on frame 14, the same frame the `ATTACK` spawns. Modelling rate
  revealed a real finding that the `Raw` gate had been hiding. It is a warning, not a blocker, so
  nothing that exports today stops exporting.
- **Write-back takes no pristine copy, unlike every other family here.** Those diff because their
  rule keys on a field the edit changes; a rate edit can only change an argument, so the user's
  file is its own baseline. It *does* refuse when the source's rate-call count disagrees with the
  editor's — a call inside a runtime branch is scanned but not modelled, and writing by position
  across that gap lands the edit on the wrong call.
- **Mutation testing found a test that proved nothing.** The first rewind-guard test asserted the
  backwards `frame()` still mapped somewhere sane, and passed with the clamp deleted: with a
  single rate window `game_frame` recomputes from that span's own origin and never reads the
  running clock. It needed a *second* rate window after the rewind to become a test at all.
- **A second one, same session:** the prefix-collision test passed against a prefix match, because
  the two real longer macros are rejected by their argument *count*. It took a synthetic
  two-argument `FT_MOTION_RATE_SYNTHETIC` to isolate the name match the parser actually relies on.
  Both are the standing "a detector tested on its own pattern" trap wearing different clothes.

**Live surface still unconfirmed in game.** Two boots have gone into it and neither reached a
`RATE hit`, for two different reasons — both worth recording, because neither was a bug in the
feature.

- **Boot 1** established only that no rule had arrived, because *neither side of the wire could
  say why*. The plugin's `rate_action` returned `None` silently with "every early return reports
  itself" written directly above it — the rule miss being the one return that matters, since a
  miss on the motion and a miss on the frame window are indistinguishable from outside. The
  editor's two early returns were silent as well, and that reason lives on the editor side, so
  the plugin's log could never have supplied it. **Third instance of this shape on this project.**
  Both now report; the editor also says in its status bar how many rules went out and whether the
  game is connected, which needs no boot at all.
- **Boot 2 was a naming collision, not a defect.** The reported "I edited the rate and it applied
  live" was `LAST_EFFECT_SET_RATE` — the per-spawn effect rate from A3, which has always worked
  live and travels on a different channel. This section had been called **Playback rate**, and
  the effect field's own code comment called it "Playback rate" too. Every log line was
  consistent with the feature working exactly as designed: `RATE bail: no rules loaded yet`
  proves the hook *is* reached by a real move, which is the one thing boot 1 could not show.
  Renamed to **Motion rate** and **Effect rate**, and both hover texts now say which is which by
  contrast.

**The check, when it next gets a boot:** kirby's down smash (`attack_lw4`, rate `0.25`) or up tilt
(`attack_hi4`, `0.6`). Edit the value in the **Motion rate** section — the status bar should say
the rules were sent — then *perform that specific move*, since only 10 of kirby's carry a rate
call and a walk cycle will never fire the hook. `RATE miss` now prints every loaded rule's motion
and frame window beside the ones the game used, so a mismatch names itself.

### [x] E3 — Camera and zoom (done 2026-08-06 — as a placement fix, not a camera panel)

`CAM_ZOOM_IN_arg5`/`_arg6`, `CAM_ZOOM_IN_FINAL_arg13`, `CAM_ZOOM_OUT`, `CAM_ZOOM_OUT_FINAL`,
`REQ_MOTION_CAMERA`, `FT_START_CUTIN`, `FILL_SCREEN_MODEL_COLOR`, `CANCEL_FILL_SCREEN`.
Lowest value of the gameplay set — mostly final-smash staging. Values and timing only; no
viewport preview.

**Measured first, and the entry did not survive it.** 14 corpus calls, not the 10 this entry
assumed — it missed `FILL_SCREEN_MODEL_COLOR` (2) and `CANCEL_FILL_SCREEN` (2). Of the eight
macros named, **not one is worth modelling**:

| macro | corpus | why not |
|---|---|---|
| `CAM_ZOOM_IN_arg6`, `CAM_ZOOM_IN_FINAL_arg13`, `CAM_ZOOM_OUT_FINAL`, `REQ_MOTION_CAMERA` | **0** | no evidence, same rule as B2 / C4 / `AFTER_IMAGE2_ON` |
| `FT_START_CUTIN` | 5 | **takes no arguments.** There is no value to edit |
| `CAM_ZOOM_OUT` | 2 | same — `(agent)` and nothing else |
| `CAM_ZOOM_IN_arg5` | 3 | **the corpus form is not a call.** See below |
| `FILL_SCREEN_MODEL_COLOR` | 2 | 12 slots on 2 calls; already carried verbatim, nothing lost |
| `CANCEL_FILL_SCREEN` | 2 | **was** deleted by the effect export — fixed without modelling it |

**`CAM_ZOOM_IN_arg5` is a decompiler artifact and would have poisoned an export.** The corpus
writes `CAM_ZOOM_IN_arg5(0, 0);` — no `macros::` prefix, no `agent`, two arguments. smash-script
declares `CAM_ZOOM_IN_arg5(agent, zoom_amount, arg2, arg3, y_rot, x_rot)`: agent plus **five**.
The corpus line cannot compile and never could; it sits two lines below `0x2508e0(-986880942,
2.1)` in the same block, which is the same tool failing the same way. Modelling this from its
only evidence would have pinned an export that does not build — precisely the AFTER_IMAGE trap.
`game_` scripts keep every line they do not understand, so all 10 of the `game_`-side calls
already round-trip verbatim today, artifact and all, which is the right answer for them.

**So the only real defect in the whole family was 2 deleted lines, and fixing it had nothing to
do with cameras.** `dolly/FinalAirEnd` frame 40 is two `CANCEL_FILL_SCREEN` calls and nothing
else. C6 carries an unmodelled line by attaching it to a spawn in its own frame block; that
frame has no spawn, so both lines were deleted by every export of that move.

**The fix is a third option `EffectWalk::end_frame` had refused to consider.** It knew that
attaching residue to a *later* frame's spawn would retime it rather than preserve it, and stopped
there — so the lines were reported as dropped. They can simply stay at the frame they were
written at and be emitted there with no call to hang from: they already arrive wrapped in their
own `if macros::is_excute(agent) { … }`, and the emitter has always been able to open a bare
block, it just had no way to be told a frame existed unless a call sat on it.

Nothing about `CANCEL_FILL_SCREEN` is modelled. Every reason C6b gave for not modelling it still
holds. The lines are copied through verbatim and warned about as verbatim, exactly like a carried
line.

**It removed twice what it set out to.** 16 lost lines → 12, 13 lossy scripts → 12: the two
`CANCEL_FILL_SCREEN` calls *and* the two bare `EffectModule::remove_screen` calls in kirby's
`FinalAirStart` / `FinalStart`, which C6b had written off as "genuine script plumbing with no
editor meaning, worth leaving reported". That was right about the meaning and wrong about the
cause — they were never lost for want of understanding, they were lost for want of a spawn to
sit beside.

**The surfaces.** Parse+IR: `to_effect_calls_reporting_losses` became
`to_effect_calls_and_residue` and no longer returns a loss list, because this walk now drops
nothing; `unexportable_effect_lines` is decided entirely from the statement tree. Export: the
emitter takes residue as a fourth argument with **no defaulted overload** — passing an empty map
is a claim, and a caller making it by accident deletes exactly what this fixed. Panel: the
verification pane derives residue from the open script, so it shows what an export would write.
Live: untouched and out of scope — these are lines the editor does not model, so there is nothing
to send. Write-back: untouched, for the same reason.

**Persistence was the half that nearly did nothing.** The export path builds from the saved call
list, not from a re-parse, so residue had to be saved with the project — `effect_frame_residue`
on `FighterMod`, the exact sibling of `effect_dropped_lines`. Unlike that field it *changes
generated code*, so a project that reloads without it exports as the pre-E3 build did: the lines
vanish, and no note describes them either, because this build stopped producing one.

**Six mutations, three of which survived the first round of tests:**

- Emitter ignores residue — caught immediately.
- **`end_frame` handed the frame being entered instead of the one being left.** Survived. The
  corpus's only spawn-less block is the *last* frame of its script, flushed by the final
  `end_frame`, so it says nothing about the two inside the walk. The wrong version exports code
  that compiles, balances its braces, re-parses to the same spawns, and plays the line 20 frames
  late. Needed a fixture with a frame *after* the residue.
- **`source_project` passing `Default::default()` instead of reading the saved field.** Survived
  the whole suite including the corpus ratchet, because every other test builds residue and calls
  from the same parse and never goes through a file.
- **The app never writing the map.** Survived. Same blind spot the sibling field hit before —
  its own test records that the export-side reporting was once "fully built and fully tested
  against a map the editor never filled". The body is now split out as
  `record_effect_script_notes` so a test can reach it.
- The carried-line warning ignoring residue — caught after the fix, which is *why* the fix
  exists: the first draft emitted those lines and said nothing about them, so a script whose only
  unmodelled line owned a frame of its own exported verbatim, silently.
- Residue never cleared when a move has none — caught.

The corpus ratchet gained a second half for the same reason. `lossy` counts what the report
*names*; a new pair of assertions counts what the export *writes*, because a change that stopped
producing residue at all would have left `lossy` at 12 and looked like success.

**Deliberately not done:** `FILL_SCREEN_MODEL_COLOR` stays unmodelled. Twelve argument slots on
two calls, one of which the dump spells `EffectScreenLayer:*GROUND` — not valid Rust. Naming
those slots would be inventing meaning, and it is already carried, so nothing is lost by leaving
it. Worth knowing: that line is why `dolly/FinalAirEnd` is *still* blocked by export verification
— carried verbatim, and verbatim from these dumps does not always compile. That is the designed
failure and it is loud, but it means the move this task was measured on cannot actually be
exported yet. **A user hitting that has to fix the line by hand.** Real, bounded, and a different
task from this one.

---

# Part 2 — Known rough edges

Not ACMD coverage. Real, already-diagnosed, and each blocked on something specific.

### [!] R1 — Instant carrier retire (blocked: needs `ITEM_MANAGER_OFFSET`)

Carrier swaps cost ~300 frames (5s) about half the time. Not a hang — the game's deferred
destruction of a dead item. `remove_auto_carrier`
([effect_reload.rs](plugins/slight_replica/src/slight/effect_viewer/effect_reload.rs)) has two
paths: a held item is removed outright and is instant; a *loose* item only gets
`retire_auto_carrier_id` (clear IMMORTAL, lifetime 0, offstage, force `ITEM_STATUS_KIND_DEAD`)
and then waits. It goes loose because the holder got hit.

The fix is `ItemManager::remove_item_from_id(manager, id)` — it exists in the skyline-smash
bindings and takes a battle-object id, exactly the shape a loose item needs. Wire it into the
`else` branch, guarded by `carrier_boma_for_id`, falling back to today's DEAD path when the
manager pointer is null, so worst case is current behaviour.

**Blocker:** it needs an `*mut ItemManager` singleton and the plugin has no
`ITEM_MANAGER_OFFSET`. Every manager here is a hardcoded build-specific text offset.
**Do not guess one — a wrong deref is a boot crash.**

To unblock: set `SSBU_DUMP_DIR` to a dump containing `exefs/main`, `pip install lz4`, then
`research/decomp/ssbu-re/nso_syms.py` parses the NSO dynsym (which still carries the mangled
`_ZN3app...` names, unlike the stripped Ghidra projects here). Resolve
`_ZN3app12item_manager22get_num_of_active_itemENS_8ItemKindE`, disassemble its prologue with
capstone using the ADRP+ADD/LDR technique `xref_scan.py` already uses, and read the global it
loads. **Validate the method by re-deriving the known `EFFECT_MANAGER_OFFSET = 0x5333920`
first — if it does not reproduce that number, do not trust the new one.**

### [x] R2 — Windows path and frame-path audit (done 2026-08-05)

The dev machine runs Eden on Linux; a chunk of the tester base is on Windows. Two bug classes
are structurally invisible here and have shipped before:

1. **Per-frame filesystem work in the plugin.** Linux serves repeated *failed* lookups from the
   negative dentry cache at ~1 µs. Windows has no negative-lookup cache: each miss is a full
   path parse through the emulator's sdmc VFS with Defender hooking the open, 20–200 µs. Work
   that is free here costs the whole 16.6 ms frame budget there.
2. **Host paths built from `dirs::home_dir()`.** `home_dir()/.local/share/...` is Linux-only
   and silently resolves to a nonexistent Windows location — and `create_dir_all` then
   *creates* it, turning a wrong path into a successful write.

**Closed 2026-08-05. The sweep is written up below. The headline finding was not the bug class
this entry predicted.**

**Half 2 (host paths) was already clean.** One `home_dir()` remains, in a Linux-only test that
exists to prove `dirs::data_dir()` still resolves to the path that was hardcoded before. No
hardcoded `.local/share` / `AppData` / `C:\Users` anywhere in `src/`. Every `is_dir()` outside
[scratch_dirs.rs](src/scratch_dirs.rs) is an iteration filter, a test assertion, or a check that
a path the *user explicitly chose* still exists — none is a probe validated by existence alone.
`is_emulator_sd_root` still requires `ultimate/`. Nothing to do.

**Half 1 (frame paths) was not clean, and the sd_poll consolidation had a hole in it.**
`poll_transactions` was called from the debuggable-server facade's `on_frame` **every frame,
ungated**, and its first statement was `ensure_slight_dirs()`. Per frame, on the game thread:

| # | Call | Path | Normally |
|---|------|------|----------|
| 1-4 | `create_dir_all` ×4 | `debug/loggers/`, `user/error_logs/`, `user/debuggables/`, `user/` | all four already exist |
| 5-7 | `exists` ×3 via `refresh_debug_logging` | `activate.txt`, `deactivate.txt`, `trace.txt` | all three missing |
| 8 | `read_to_string` via `load_disabled_subsystems` | `debug/off.txt` | missing |
| 9 | `read_dir` | `user/debuggables/` | empty |

**Nine filesystem operations per frame, every one of them redundant** — four `create_dir_all` on
directories created at boot, four failing lookups, and a `read_dir` of a directory that is empty
except in the seconds after someone drops a file into it by hand. That is precisely the shape
`slight::sd_poll` was built to eliminate, at roughly the same magnitude as everything that moved
there. It survived because the consolidation moved the *pollers* and never audited the *callers*.

- **Three separate comments asserted the invariant this call site broke**, and all three still
  read as correct: `sd_poll`'s module doc ("they all live here now"),
  [agent_extender](plugins/slight_replica/src/slight/agent_extender/mod.rs:218) ("Every SD-card
  poll in the plugin happens here"), and `refresh_debug_logging`'s own ("Called from the
  throttled SD poll tick, not per frame") — which was true of one of its two callers.
- **Fixes.** `ensure_slight_dirs` is boot-only now, guarded by an atomic, which removes eight of
  the nine with no behaviour change (nothing recreates a directory a user deletes mid-session,
  and `read_dir` on a missing directory already returns the empty answer). The `read_dir` is
  throttled to one poll per 10 frames. It stays in the facade rather than moving into
  `sd_poll::tick` because it returns edits to apply and that tick returns nothing.
- **A coupled constant went with it.** `MAX_TRANSACTION_ATTEMPTS` counted one attempt per *poll*
  and was documented "~10s at 60fps", which silently assumed a poll every frame. Throttling
  alone would have stretched that window to 100 seconds. It is 60 now, next to a
  `TRANSACTION_POLL_EVERY` the caller gates on, so the two cannot drift apart unnoticed.
- **Not a live-edit regression.** Transactions are the hand-dropped-file escape hatch — the
  editor never writes one (nothing in `src/` mentions `debuggables`); live edits go over TCP via
  `poll_tcp_edits`, which is untouched and still runs every frame.
- **Regression guard, aimed at what actually went wrong.**
  `no_plugin_frame_path_reaches_the_filesystem_ungated` in [scratch_dirs.rs](src/scratch_dirs.rs)
  fails if any of the plugin's 32 frame entry points reaches a filesystem-touching function
  without a throttle between them. Both sides are derived by scanning plugin source — no
  hand-maintained list to rot. **A ratchet on the number of filesystem call sites, which is what
  I first reached for, would have been green through this entire bug:** nothing was added, a
  long-standing I/O function was simply called on the wrong schedule. The property that catches
  it is cadence, not count. Verified by reverting the fix and watching it go red.
- **What is still not verified, and cannot be here.** The guard is one hop deep and blind to
  chains through an intermediate, to trait objects, and to function pointers. It is a tripwire on
  a shape that has now gone wrong twice, not a proof of absence. Frame timing on Windows remains
  the only real oracle, so **this class stays open in practice even though the entry is closed**
  — re-run the sweep when a tester reports frame drops rather than assuming the guard settles it.
- **Also noticed, not fixed (deliberately).** Four debug writes to hardcoded `sd:/…txt` paths are
  *not* behind `trace_enabled()` — `hook_effect_off_kind`, `track`, `handle_kill_hash`,
  `install_hooks`. Each is bounded (32 or 48 distinct keys per boot, or one-shot), so none is a
  frame-budget risk, but they ship enabled and write to the SD root. Worth folding into the trace
  gate if that area is touched again; not worth a commit of its own.

### [x] R5 — The move list shows only attacks (done 2026-08-06)

**Found while trying to verify D1f live: the walk cycle cannot be opened in the editor at all.**
[app.rs:1755](src/app.rs:1755) filters the motion list to six substrings — `attack`, `special`,
`throw`, `catch`, `cliff`, `final` — before a move is even considered. `walk_middle`, `landing`,
`dash`, `jump`, `wait`, `entry`, `win`: none survive it.

Measured against the corpus, this is most of D1's own subject matter:

| | total | reachable |
|---|---|---|
| scripts with sound | 301 | **107 (35%)** |
| sound calls | 610 | **260 (42%)** |

Per macro it is worse. Unreachable: `PLAY_FLY_VOICE` **5 of 5**, `PLAY_DOWN_SE` 9 of 11,
`PLAY_LANDING_SE` 9 of 12, `PLAY_STEP_FLIPPABLE` 28 of 38. The very first sound the plugin
captured in a real boot was a `PLAY_STEP_FLIPPABLE` from walking — a call whose move the editor
cannot open.

**The filter is not a bug in itself; it is a perf guard that became a capability limit.** Its own
comment says so: "Filter early to avoid reading files for non-attack moves", i.e. avoid parsing a
`.nuanmb` per move for frame counts. That was correct while the editor only did hitboxes, because
non-attack moves mostly have none. Sound landing in D1c changed what "relevant" means and nothing
failed, because the moves simply never appear. **This is the `Raw`-filter trap from B4 and D1c in
a third place: a filter that is correct under one assumption becomes a silent restriction when a
new family arrives.** Added to the traps list.

- **Work order:** widen the list to every motion, keep the frame-count read lazy so load time
  does not regress, and group the result — 458 distinct move names in the corpus is not a flat
  list anyone can use.
- **Categories are derived from the corpus, not invented.** Ground attacks, aerials, specials,
  grabs and throws, ledge, Final Smash, movement, jumps and landing, idle and crouch, defense,
  damage and knockdown, items, taunts and results, situational.
- **Item weapon movesets are their own cluster and are large.** `scope_*` alone is ~50 names
  (Super Scope), plus `bat_swing*`, `club_swing*`, `death_scythe_swing*`, `l_gun_shoot*`,
  `steel_diver_shoot*`, `f_flower_shoot*`, `genesis_*`, `magic_pot_*`, `drill_shoot*`. Folding
  these into "Items" beside `item_light_get` is the difference between a usable list and a wall.
- **Kirby copy abilities appear on two different axes, and only one is in scope.** Names like
  `cloud_special_n` / `cloud_special_air_n` sit in the ordinary motion list and are a category
  here. The *other* axis is bigger: Kirby has **209 motion directories** (`copy_koopa_cap`,
  `copy_shulk_sword`, `*body`, …) and the editor only ever loads `motion/body/c00`. That changes
  which `motion_list.bin` is read and how a move is keyed, so it is **its own task** — see R6.
  Do not let it ride along here.
- **Test bar:** a corpus test over every distinct move name asserting the uncategorised bucket
  stays small and that named examples land in the right group. A categoriser silently dumping
  everything into "Other" is the failure mode, and it looks like success.

**Done.** [move_kinds.rs](src/move_kinds.rs) holds an 18-group `MoveGroup` with an explicit
`ORDER` for display. The six-substring filter is gone and the per-move frame-count read moved
into `select_move`, so widening the list to all 458 names did not regress load time. Copied
specials are their own group rather than a corner of Specials, and the item weapon movesets fold
into one "Items & weapons" group.

- **The corpus test caught two errors the hand-written rules made**, which is the whole reason it
  is a corpus test and not a table of examples. Jabs are spelled both `attack_11` and `attack_1`
  in the wild, and `_swing` weapons had to be matched by *shape* rather than against an
  enumerated list of weapon names — an enumerated list being the exact trap this backlog keeps
  hitting.
- **Two categorisers already existed and this consolidated them rather than adding a third.**

### [x] R6 — Kirby copy abilities live in motion directories the editor never loads (done 2026-08-08)

Split out of R5. The local extracted Kirby data has **208** motion parts. The body motion list
contains the copied-special entries (`cloud_special_n`, `cloud_special_air_n`, and the other
donor-prefixed motions), but their animation references point outside `motion/body/c00`:
`cloud_special_n` names `cloudd00specialn.nuanmb` under `motion/cloudbody/c00`, for example.
The old editor scanned only `body/c00`, so the ACMD panel could be correct while the viewport
showed no animation for the same move.

This measurement also settled the scope. ACMD remains fighter-level script data; the motion list
already carries the per-move animation hash, and the local script cache has no directory-qualified
ACMD source to merge into the move. R6 is therefore an animation-resolution rough edge, not a new
ACMD family or a reason to invent a second script key.

**Done.** Move-list loading now indexes every `motion/<part>/cNN` directory once and resolves the
animation hash over the complete `.nuanmb` filename before using the body-only compatibility
fallback. That makes copied specials and other sibling-part animations available to the existing
viewport without changing ACMD capture, live rules, exports, or source write-back. The README
describes the required extracted-data layout and the behavior.

The checked-out archive contains a `copy/` directory in the remote script index, but it was not
part of the cached top-level script files and was not used to claim additional ACMD coverage. Any
future work on those nested source files remains a separate measured source-index task.

### [x] R7 — A live capture threw away everything but hitboxes and effects (done 2026-08-06)

Found by the first person to press **⟳ Live** on a move with no hitboxes. The button was
clickable, the status said "Capture has no ATTACK/EFFECT lines for this move yet", and nothing
changed — while the plugin had captured two `PLAY_STEP_FLIPPABLE` calls from the same walk.

`load_from_captures` returned early on `hitboxes.is_empty() && effects.is_empty()`, and both the
hurtbox adoption and the sound adoption sit *below* that line. The guard was an accurate summary
of what a capture produced when it was written; hurtboxes arrived in B4 and sounds in D1f, and
neither updated it. **A walk cycle has no hitbox and no effect, so its whole capture was
discarded with a message naming the two families it had not looked for.**

**This is the third instance of one shape in two days**, and it is worth naming as a family:
a filter or guard that is a correct summary of "everything that exists" at the time of writing,
and becomes a silent drop when a new family arrives. The other two: the move list's six-substring
filter (R5) and the `Raw`-only retention in `rebuild_script_from_hitboxes` (B4). None of them
failed loudly, and none was caught by a test, because in every case the code was *right* when
written and nothing re-examined it.

- **Fixed** by building the hurtbox script and the sound script *before* the check, and refusing
  only when all four are empty. The status line now counts sounds too, so "nothing happened" is
  distinguishable from "nothing of the kind you were looking at".
- **The decision is now `nothing_to_load(hitboxes, effects, hurt_stmts, sounds)`**, a free
  function, because the inline version was unreachable by any test — the same move
  `sound_rules_for` and `adopt_captured_sounds` needed for the same reason.
- **The refusal message is asserted to name every family**, which is the part that guards the
  *next* one: a fifth family added without updating the check will fail
  `a_capture_with_nothing_in_it_is_refused_and_names_every_family` only if its name is expected
  there, so the test is a reminder rather than a proof. Said plainly in the test's own comment.
- Two mutations, both caught: restoring the two-family guard fails three tests, and reverting the
  message to the old wording fails the fourth.

### [x] R8 — Switching moves kept the previous move's sounds (done 2026-08-06)

Found immediately after R7, by opening a second move. The sounds from the first one stayed on
screen, and a **⟳ Live** on the new move did not replace them.

`select_move` clears the hitboxes, the game script, the effect script and the effect list. Sound
arrived in D1c and was never added, so `sound_script`, `sounds` and `sounds_pristine` survived the
move change. **Two failures compounded**: the stale list was displayed as if it belonged to the
new move, *and* `adopt_captured_sounds` declines to overwrite a non-empty sound script — a
deliberate rule, so a capture cannot clobber a fetched script — so the new move's live capture was
then refused and the old data kept looking authoritative.

**The fourth guard of this shape in two days**, after the move-list filter (R5), the capture
early-return (R7), and `rebuild_script_from_hitboxes` (B4). Every one is an enumeration of "all
the things that exist" that was accurate when written. See the traps list.

- **Fixed** as `clear_move_state(&mut AppState)`, a free function, so the enumeration can be
  asserted. It goes through `set_script` rather than assigning the field, because that also
  derives `hurtboxes_pristine` and `attack_mods_pristine` and reimplementing it here is one more
  place to drift.
- **Deliberately not `AppState::default()`**, which would also drop the fighter, the loaded
  labels, the project and the edit log. The test asserts a label survives, so a later
  simplification to `default()` fails rather than silently resetting the session.
- **`switching_moves_clears_every_per_move_field` is an exhaustiveness test the compiler cannot
  write.** A per-move field added to `AppState` has to be cleared there and asserted there, and
  nothing else will catch it. Both mutations caught: dropping the sound clear fails it and fails
  `a_cleared_move_accepts_a_captured_sound_script_again`.

### [x] R9 — A missing script is cached as its own HTTP error page (done 2026-08-06)

Found while counting the corpus for E2. Three files in the 461-file cache are 14 bytes long and
contain the text `404: Not Found`:

```
dolly/SpecialBStart.txt   dolly/SpecialBAttack.txt   dolly/SpecialBAttackW.txt
```

`fetch_script_body` returns `HTTP.get(&url).send()?.text()?` — **`send()?` only fails on a
transport error, so a 404 is a perfectly successful request whose body is the error page.** That
body is then written to the cache and returned as `Ok(body)` to six call sites, every one of which
treats it as script source.

**The cache doc comment says this is deliberate** — "bodies (including `404: Not Found` misses)
are stored … so fighter-wide scans only hit the network once per move ever" — and for the *scan*
path that is right: the move genuinely does not exist upstream and re-asking every session is
waste. The bug is that the same value is handed to the *open a move* path, which cannot tell a
miss from a script. Caching the miss is correct; **representing it as a body is not.**

- **Symptom in the editor:** opening one of those moves shows a one-line script whose content is
  the words `404: Not Found`, presented as if the move had a script with one unmodelled line.
- **Symptom in the export, which is worse:** `emit_stmts` writes `AcmdStmt::Raw` lines out
  verbatim, so exporting that move emits `404: Not Found` into a generated `.rs` file. That is
  the [verbatim escape hatch](#traps-that-have-already-cost-real-time) trap again — a Raw line is
  trusted to be Rust, and here it is an HTTP error page.
- **Fix shape:** check the status code at fetch time and cache a miss as an *empty* file, and
  treat both empty and a legacy `404: Not Found` body as "no script" on the read path, so the
  three files already on disk are handled without asking anyone to clear their cache.
- **Test bar, and this is the part to get right:** a negative test ("the miss is not parsed as a
  script") needs its paired positive — a real body through the same path that *does* parse —
  or it passes with the bug restored. See the standing trap.

**Done.** `fetch_script_body` checks the status and reports a miss as an empty body;
`script_source_from_body` normalises a legacy `404: Not Found` file the same way, so the three
already on disk are handled without anyone clearing their cache.

- **A miss normalises to an empty body, deliberately not to `None`.** `None` means "not cached"
  to the fighter-wide scan, so the obvious fix would have sent every known-missing move back to
  the network on every scan — undoing the caching this path exists for, and failing nothing.
  `a_cached_miss_still_counts_as_fetched_so_a_scan_does_not_re_request_it` exists for that
  specific wrong fix, and it is the mutation that proves it: implementing the miss as `None`
  fails it.
- **The cold and warm paths were about to disagree.** `fetch_script_body_cached` returned the
  fetch result *unnormalised* on a cold cache and the normalised one on a warm cache, so the
  status check was the only guard on the first fetch of a move and the normaliser the only guard
  afterwards. Both now go through the one function, which is what makes the next point tolerable.
- **Named coverage boundary: neither network function is under test**, because both need a
  network and there is no injectable client. What *is* tested is the decision they feed — that
  is why the cold path was routed through it rather than left to the status check alone. A
  regression in the status check is now caught downstream instead of shipping.
- **Three mutations, all caught**, and each by a different test: blinding the detector fails the
  read and export tests, returning `None` for a miss fails the scan test, and returning an empty
  body unconditionally fails only the *positive* half — which is exactly the hole a lone negative
  test would have left.
- **Follow-up, small: the corpus test helpers still read the cache raw.** Five of them build
  their own file lists rather than going through `cached_script_body`, so those three error pages
  are still parsed as one-`Raw`-line scripts by the oracle. Harmless today — they round-trip —
  but it inflates any count taken over the cache, which is how this was found in the first place.

### [x] R10 — A live capture had no motion rate, and could not say what else it dropped (done 2026-08-06)

Reported as "the live capture failed to get hitboxes, hitbox tuning, and motion rate — the GitHub
one has it", on `attack_lw4`, which carries all three.

- **Motion rate: a real gap, and mine.** `rate_hooks` was written from the override end and never
  grew the other half — it never called `record`, so no `FT_MOTION_RATE` ever entered the capture
  stream. Every other family here records on the same line as it acts. A capture of a
  rate-carrying move therefore came back with no rate, and an export written from that capture
  would have dropped the call. Fixed on both sides: the plugin records it, and the rebuild places
  it as a **bare top-level statement** — all 17 corpus calls are bare, and wrapping one in an
  `is_excute` the source never had is a behaviour change as well as a round-trip failure.
- **Hitbox tuning: not a gap.** `ATK_POWER` has always been recorded by the plugin and read by an
  arm in the rebuild, and `LuaArgWire::as_f32` already accepts an `Int`. Checked rather than
  assumed — the first fix written for it was a duplicate `match` arm, caught by the compiler as
  unreachable.
- **The real defect was that none of this was observable.** The post-capture status line named
  hitboxes, effects and sounds, with a comment reading "Names every family" — while three of the
  six went uncounted. So a capture that dropped tuning and rate was indistinguishable from one
  that never contained any, and the report could not tell the two apart either. **Fifth instance
  of the enumeration trap**, and the first where the enumeration was in a *message* rather than
  in a guard, which is why no test caught it: nothing was wrong with the code it described.
- Now counts all six and **prints the zeroes**, because "0 tuning call(s)" is the answer to the
  question, not noise to be trimmed. The `state.script.stmts.is_empty()` refusal — which keeps a
  fetched script rather than replacing it with a capture-derived one — also says so now when it
  actually discards something, instead of being silently correct.
- **Consolidated rather than extended.** `hurtbox_script_from_captures` became
  `script_from_captures`; adding a second and third builder beside it would have been three
  copies of the same frame bucketing and ordering rule. Adding a family is now one `match` arm.

### [x] R11 — A charged smash attack lost every call after its charge (done 2026-08-06)

**Diagnosed by the user, from the symptom pattern, after I had guessed wrong three times:** "the
tilt attack looks right, but the smash attack has similar issues… down smash is an attack that has
a loop section during which it is charged."

`mark_capture_motion` refuses to open a second run for a `(kind, motion)` already claimed, and a
claim lives until the editor clears captures. Both it and `capture_tick` decide a playback is over
with the same test — `w.motion != motion || frame + 0.5 < w.frame`. A smash attack's hold trips
it: `attack_lw4` sets `START_SMASH_HOLD` on frame 5, and the charge either rewinds the motion
frame or parks in another motion. The capture was declared finished there, and **every later call
was then dropped by the claim**, silently.

| `attack_lw4` | frame | outcome |
|---|---|---|
| `FT_MOTION_RATE` ×2 | 0, 4 | before the hold — captured |
| `START_SMASH_HOLD` | 5 | capture declared over |
| `ATTACK` ×2 | 10 | **dropped** |
| `ATK_POWER` ×2 | 15 | **dropped** |
| sounds | after 5 | **dropped** |

- **Fixed by resuming the run when the claim is held by the same object.** A charge is one
  performance of one move and belongs in one capture. A genuine repeat — a jab thrown twice —
  folds into the lines already held, because the dedupe key is (motion, frame, func, args). A
  claim held by a *different* object is a real conflict and is still refused.
- **`capture_tick`'s early end marker is deliberately left alone.** It is what ends a capture of a
  genuinely looping motion (`walk`, `run`), which never reaches `end_frame`. The premature marker
  now self-heals: the run resumes, and the real end pushes a second marker carrying the complete
  capture.
- **Why this took four rounds.** The symptom moved every time — first "hitboxes, tuning and motion
  rate", then "hitboxes, tuning and sound" — because *which* families went missing depended only
  on whether they ran before or after frame 5. Reading that as a per-family bug is what sent me
  into three different families' code in turn. **The invariant was "everything after the charge",
  and only the user saw it**, because they knew down smash charges and I was reading the families
  in isolation. **When a symptom set changes between reports but the move does not, the thing they
  share is a position in the timeline, not a family.**
- Guarded from the editor by `the_plugin_resumes_a_capture_claim_held_by_the_same_object`, which
  reads the plugin's source. Weak — it pins text, not the linked `.nro` — but the property is
  invisible from this crate and its failure is silent by construction: a dropped capture line
  looks exactly like a call the game never made.

### [x] R12 — Exporting from a live capture silently drops the branches that were not taken (done 2026-08-07)

**Not a capture bug — a provenance hazard, found while explaining one.** `effect_attacklw4`
chooses between `sys_whirlwind_l` and `sys_whirlwind_r` on `SO_VAR_FLOAT_LR`, the facing
direction. Exactly one runs, so a capture holds exactly one, correctly. The GitHub fetch holds
both because it is the source.

That difference is fine on screen and **not fine on export.** With provenance "Live capture" the
effect list is the whole model — there are no `Raw` lines to carry through, because the branch was
never parsed, it was never *there*. So exporting a move loaded from a capture writes a script with
one arm of the branch and no condition, and the mod then plays the wrong whirlwind in half of all
cases. Same for any ground/air, flag, or costume branch, and `RawBlock` is common in the corpus:
**107 of 432 functions carry at least one unmodelled line** (measured for E2).

- **The user cannot currently tell.** "Live capture" appears as a provenance string; nothing says
  it is a *partial* view of a branching script.
- **Cheapest useful fix:** when a cached GitHub script exists for the same move, compare its
  branch count against the capture and warn on export — the editor already fetches it for
  `capture_vs_script_offset`, so the data is at hand and needs no network.
- **Do not try to merge the two sources automatically.** Which arm a captured call came from is
  not recoverable, so a merge would have to guess where to reinsert it, and a wrong guess writes a
  condition the author never had. Warning is honest; merging is not.
- **Implemented 2026-08-07.** A live capture now counts runtime branches in the cached source,
  records a provenance warning beside the move, carries it through `modproject.json`, and
  includes it in the generated export warning channel only when that move is actually shipped.
  The older edit-log export also keeps the warning in its status line. No source arms are merged
  or guessed; the warning is the explicit boundary for every current capture/export surface.
- **Related and already true:** performing the move under both conditions in one capture window
  records both arms (R11 made repeat performances land in the same run), but they arrive as two
  unconditional calls, not as a branch — which is *worse* than one arm, because the export would
  then play both at once.

**Confirmed in practice by R13**: capturing both arms was tried and produced "a whole bunch of
junk effects". The capture is now one performance by construction, so this is no longer reachable
by accident — but an export from a captured branching move still writes one arm with no condition,
which is what this entry is for.

### [x] R13 — R11's fix made every later performance pile into one capture (done 2026-08-06)

**A regression from R11, reported one message later: "doing both directions adds a whole bunch of
junk effects into the timeline."**

R11 resumed a capture run whenever the same object re-entered a claimed motion, and justified it
in its own commit message: *"a genuine repeat — a jab thrown twice — folds into the lines already
held, because the dedupe key is (motion, frame, func, args)."* **That is false for exactly the
move R11 existed to fix.** A charged smash releases at a different motion frame every time, so the
`frame` in the key differs between performances, nothing collapses, and each repeat stacks another
copy of every spawn onto the timeline.

- **The claim now records whether its playback reached `end_frame`.** Suspended part-way (a
  charge) → resume the run. Finished → open a fresh one. The editor already reads only the newest
  run per motion (`latest_run_for`), so the last complete performance wins and **a capture is
  always one performance** — which is also the only thing a script can faithfully represent.
- **The flag lives on the claim, not the watch entry**, because the watch entry is dropped the
  instant the motion frame steps backwards. By the time the next playback records a line there is
  nothing left to say whether the previous one finished or was merely held; only the claim
  outlives that gap.
- **Applied as a value spent after the lock is released.** Setting it inline would have been the
  only nested acquisition of `MOTION_WATCH` → `CAPTURE_CLAIMS` in the plugin, and a lock-order
  hazard on a game thread is a frozen console, not a failed test.
- **Both halves are pinned now, and each alone is a bug that shipped**: dropping `!h.ended` loses
  everything after a charge (R11); keeping the resume without the claim flag accumulates
  performances (R13). `the_plugin_resumes_a_capture_claim_held_by_the_same_object` fails under
  either mutation.
- **The lesson is about the justification, not the code.** R11's reasoning named the dedupe key
  and asserted it would collapse repeats — without checking that the key's `frame` component is
  stable across performances, which for a *charged* move is exactly what it is not. A claim about
  why a change is safe is a claim to be measured like any other. This one was written confidently
  in a commit message and was wrong within the hour.

### [!] R3 — Robust Skyline 13.0.4 hook (blocked: needs a physical Switch or proven hook evidence)

`plugins/slight_replica/src/slight/systems/skyline_hook.rs:66` carries the only TODO left in
the source: the current hook is a workaround to keep the core effect viewer usable. Wants
either a proper 13.0.4 hook or a run on real hardware.

### [x] R4 — Guard against the double-plugin footgun (done 2026-08-05)

Skyline loads **every file** in `romfs:/skyline/plugins/` as a plugin regardless of extension.
A `.bak` beside the real `.nro` runs two full copies: double ACMD hooks, double per-frame
ticks, two servers contending for port 7878, both overwriting the same `sd:/slight/` diag
files. Symptom is a hard 60→30 fps drop on entering training mode. It cost ~6 rounds of
misdiagnosis once, because it invalidates every A/B test and makes `diag.txt` show the old
build's header.

**Two halves, and only one of them is verifiable from this machine. Both were done; the entry
says which is which.**

**Deploy script (verified).** `deploy_plugin.py` scans the target plugins directory before
copying anything and refuses, exit 2, if a second copy of this plugin is already there. It
matches on **content, not filename** — the bytes `sd:/slight/diag.txt`, the path constant every
build compiles into rodata — because the filename is exactly the thing that varies: `.bak`,
`.old`, `lib_effect_viewer (1).nro`, a hand-renamed known-good build. Confirmed against a real
release `.nro`. `--remove-strays` deletes them instead; nothing is deleted without it, because
one of those files is quite plausibly a build somebody parked there deliberately.

An **unrelated** Skyline plugin in the same directory is reported and does not block the
deploy. A guard that fires on a legitimate setup is one users route around, and then it
protects nobody.

**Plugin (not verified — needs hardware).** The bind failure was already logged, as
`SRV bind :7878 rc=-1`: one lowercase line among thousands, which nobody reads as "you have two
plugins installed". It is now a banner naming the cause, the remedy, and — the honest part — a
**second check the reader can run**, because a bind can fail for other reasons and the banner
does not assert its own diagnosis. `SRV thread entered` appears once per loaded copy; two of
them is the proof. The banner also explains the tell that misled us the first time: both copies
truncate and rewrite the diag header during plugin init, so whichever loads *last* wins the
`build=` line, and a reader who just deployed build X sees build Y and concludes their deploy
did not take.

Only the copy that *loses* the race can see any of this, so the banner appears exactly once no
matter how many copies are loaded. That is sufficient — it is one file.

**Tests:** [tests/deploy_plugin.rs](tests/deploy_plugin.rs), a new integration test that drives
the real script in a throwaway directory. It lives on the host side, in Rust, because
`cargo test` is the gate this project actually runs — a Python test suite nothing invokes is a
comment. Four mutations, each caught by the test that claims the property: never refuse; drift
the marker; refuse *after* copying (breaks the "a refusal changes nothing" claim); treat
unrecognised files as strays.

The `refuses_the_deploy_and_changes_nothing` assertion is a negative, so
`a_clean_plugins_directory_deploys` is there as its paired positive — otherwise a broken script
path or a Python syntax error would look exactly like a successful refusal.

`the_marker_the_script_greps_for_is_still_in_the_plugin_source` is the one that matters most
and is the weakest. Every other test synthesises its own fixture bytes containing the marker,
so if the plugin ever stops writing `sd:/slight/diag.txt` they all stay green while the scan
matches nothing real and the guard goes quietly dead. That test asserts the literal is still in
`diag.rs` and still matches the script's copy. It does **not** prove the string survives into
the linked `.nro` — it does today, checked by hand against a release build, but a built plugin
does not exist in a fresh clone and a check that skips when the artefact is missing would pass
vacuously in exactly the case that matters.

**Not done:** the plugin cannot detect a second copy that *wins* the race, and neither half has
been seen on hardware. The deploy script closes the path a user actually walks; the banner is
for a directory populated by hand. Documented in the plugin README under Install.
