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
- Pyfa oracle + compare + frozen expectations; `cargo test` = 249 cases / 13 812 values green.
- fleet.booster_fits, projected kind `fit`, projected-module charges, incoming remote reps / neuts / nos /
  cap transfers, Pyfa missile range formula (all oracle-verified, contract v1.1–v1.3).
- CI workflow (downloads dataset release, builds, tests).
- Benchmarks: 9–13× faster than Pyfa per calculation.

## Bugs found and fixed via the oracle (this session)
skill self-bonuses (patch 0001), structure skill/implant rules, security modifier, T3D default mode,
burst value source (module, not charge), MJD sig unpenalised, BCS → pilot missileDamageMultiplier,
spooled volley, nosferatu cap income, capsim heap tie-break order, RAH simulation, bastion hull resists,
fighter abilities/squadron cap, untrained skills present at level 0.

## Known gaps / next
- Not covered yet: projected fighters, ECM jam chance, sustainable tank (Pyfa `sustainableTank` when cap
  unstable), drone control range/drone application, fighter abilities other than damage, booster side effects.
- Remaining no-modifierInfo effects (inventory in eve-fit-docs docs/03): many are activation-only;
  each needs a patch or special + oracle case.
- Perf: all published skills are instantiated (≈1 ms floor); cache skill-only modifiers per skill-set.
- WASM build + HTTP server (`serve-http`) not done yet.
