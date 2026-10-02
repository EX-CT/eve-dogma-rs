# PROGRESS — eve-dogma-rs

Updated: 2026-10-03 (Asia/Shanghai)

## Done
- Dataset loader (gz JSON, sha256 = manifest `sha256_json`), en/zh name lookup.
- FitRequest/FitStats (schema v1), CLI (`calc`, `batch`, `serve-stdio`, `eft`, `search`, `type`, `meta`, `bench`).
- Engine: data-driven modifiers, CCP operator order, stacking buckets, caps, all item kinds, specials
  (prop mods, MJD, slot/hardpoint modifiers, bursts, projected ewar, RAH, structures, T3D default mode,
  bastion hull resists unpenalised, missile pilot multiplier, nos income, fighter default abilities).
- Stats: resources, offense (turrets, missiles, smartbombs, vorton, spool, drones, fighters, vs target profile),
  defense/EHP/tank, capacitor sim, navigation, targeting, drones, validation, attribute dumps.
- EFT import/export incl. mutation blocks.
- Pyfa oracle + compare + frozen expectations; `cargo test` = 207 cases / 9 827 values green.
- CI workflow (downloads dataset release, builds, tests).
- Benchmarks: 9–13× faster than Pyfa per calculation.

## Bugs found and fixed via the oracle (this session)
skill self-bonuses (patch 0001), structure skill/implant rules, security modifier, T3D default mode,
burst value source (module, not charge), MJD sig unpenalised, BCS → pilot missileDamageMultiplier,
spooled volley, nosferatu cap income, capsim heap tie-break order, RAH simulation, bastion hull resists,
fighter abilities/squadron cap, untrained skills present at level 0.

## Known gaps / next
- Oracle does not cover fleet boosts from other fits, projected fighters/remote reps/neuts/ECM, or
  sustained remote tank; engine has partial support (see warnings in output).
- Not compared yet: weapon range/tracking/application numbers, drone control, fighter abilities other than damage.
- Remaining no-modifierInfo effects (inventory in eve-fit-docs docs/03): many are activation-only;
  each needs a patch or special + oracle case.
- Perf: all published skills are instantiated (≈1 ms floor); cache skill-only modifiers per skill-set.
- WASM build + HTTP server (`serve-http`) not done yet.
