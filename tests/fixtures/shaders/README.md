# BNSH shader fixtures (local only)

`.bnsh` and other game asset files are **not** committed (copyrighted Nintendo content).

## Auto-sync

When tests need shaders, `ensure_shader_fixtures("samus")` copies missing `.bnsh` files here from:

1. Editor **data root** → `{data_root}/effect/fighter/{name}/ef_{name}.eff`
2. Override: `HITBOX_EFFECT_EXPORT=/path/to/export/effect`
3. EffectConverter PTCL dump cache: `~/.cache/hitbox-editor/ptcl-dumps/**/Shader.bnsh`

Files are named `{emitter}_{shader_key}.bnsh` (e.g. `flare1_5740678a2aa5959f.bnsh`).

Manual sync (optional):

```bash
./tools/export_shader_fixtures.sh samus
```

Tests skip gracefully when no export is configured (e.g. CI without game data).

**Do not commit `.bnsh` files** — they are copyrighted game assets. Local fixtures under
`tests/fixtures/shaders/` are gitignored and auto-synced when tests run.
