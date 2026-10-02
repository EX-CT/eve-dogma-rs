# eve-dogma-rs

Stateless, deterministic **EVE Online fitting engine** in Rust (part of the EX-CT open fitting tool,
design docs: [EX-CT/eve-fit-docs](https://github.com/EX-CT/eve-fit-docs)).

* **Input:** one JSON `FitRequest` (ship, modules + charges + states, drones, fighters, implants, boosters,
  skills, projected effects, fleet buffs, environment, damage pattern, target profile, overrides, options),
  see `schema/fit-request.schema.json` in eve-fit-docs.
* **Output:** one JSON `FitStats` (resources, slots, offense, defense, tank, capacitor simulation,
  navigation, targeting, drones, validation violations, optional attribute dumps). The same request
  always produces byte-identical output.
* **Data:** a compact dataset built from CCP's official SDE by
  [EX-CT/eve-sde-pipeline](https://github.com/EX-CT/eve-sde-pipeline) (`dataset-<build>.json.gz`, release assets).
  Modifiers come from the SDE `modifierInfo`; effects CCP ships without it are covered by
  small data patches in the pipeline and a few documented engine specials.

## Quick start

```bash
gh release download -R EX-CT/eve-sde-pipeline --pattern 'dataset-*.json.gz'
mv dataset-*.json.gz dataset.json.gz            # or set EVE_DOGMA_DATASET
cargo build --release
./target/release/eve-dogma eft tests/fits/exct_rifter.eft --calc --skills 5   # EFT -> stats
./target/release/eve-dogma eft tests/fits/exct_rifter.eft > req.json          # EFT -> FitRequest
./target/release/eve-dogma calc req.json                                      # FitRequest -> FitStats
./target/release/eve-dogma search "Hammerhead"                                # en + zh names
./target/release/eve-dogma serve-stdio      # JSONL RPC: calc | eft_parse | eft_export | search | type | meta
./target/release/eve-dogma bench req.json -n 2000
```

Library: `eve_dogma::calc(&Dataset, &FitRequest) -> serde_json::Value` (pure; no I/O, clocks or globals).

## What is modelled

Data-driven dogma (all CCP operators incl. PostPercent/PostAssign, per-operator stacking penalty buckets
with exempt categories, min/max attribute caps, skill/ship/module/charge/implant/booster/mode/subsystem
modifiers), AB/MWD/MJD, T3D modes (default mode like the client), T3C subsystems (slots/hardpoints),
structures (pilot skills/implants ignored, power state, security modifiers), mutated modules/drones,
spool-up weapons, missiles (pilot `missileDamageMultiplier`), drones, fighters (Pyfa default abilities),
smartbombs/vorton, local reps incl. AAR paste, passive shield regen, Reactive Armor Hardener adaptation,
capacitor simulation (Pyfa-compatible event simulation incl. injectors, nosferatu income, staggering),
local command bursts and explicit fleet buffs, projected webs/TPs/damps/sebos/drones with range falloff and
resistances, validation (CPU/PG/calibration/bandwidth, slots, hardpoints, canFitShip*, rig size,
max group fitted/online/active, charge compatibility, skill requirements), EFT import/export incl. mutations.

## Accuracy: Pyfa oracle

`oracle/pyfa_oracle.py` runs Pyfa's eos engine headless as a **black box** (GPL-3.0 test tool, never linked).
`oracle/compare.py` builds each case (EFT in `tests/fits/` or JSON case in `tests/cases/`), runs both engines
and compares 48 metrics. `WRITE_EXPECTED=1` freezes Pyfa's numbers into `tests/oracle/pyfa_expected.json`,
which `cargo test` checks (no Python needed in CI).

Current: **207/207 cases, 9 827 values match Pyfa** (rel. 1e-4) — 101 dogma-engine community/regression fits,
24 hand-written fits (frigates → titans' little brothers: BS, HAC, T3C, T3D, marauders in bastion, logi,
carriers/supercarrier fighters, command ships, mining), 82 JSON cases (skills 0/2/3/4, damage patterns, RAH
profiles, reload, projected webs/TP/damps/web drones). 5 metrics are recorded as explained divergences
(Pyfa data older than SDE, invalid fits, structure power state) — see `KNOWN` in `oracle/compare.py`.

## Performance (same box, 1 core, all-V skills, including capacitor simulation)

| fit | eve-dogma | Pyfa (warm) | speed-up |
|---|---|---|---|
| Rifter | 1.15 ms | 10.8 ms | 9.4× |
| Vexor | 2.32 ms | 31.2 ms | 13.4× |
| Tengu | 1.23 ms | 15.3 ms | 12.5× |
| Nidhoggur | 1.48 ms | 14.8 ms | 10.0× |
| Hyperion | 1.35 ms | 17.1 ms | 12.7× |

Dataset load: ~150 ms (once per process). Pyfa first calculation: ~390 ms.

## License

LGPL-3.0-or-later (`LICENSE`, plus `LICENSE.GPL-3.0` which it incorporates). The RAH adaptation and the
capacitor simulator follow the algorithms of Pyfa's `eos` (LGPL-2.0-or-later). EVE Online data © CCP hf.,
used under the CCP developer license; this project is not affiliated with CCP.
