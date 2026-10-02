# Engine-owner review: eve-sde-pipeline `patches/proposed/` (dataset sde-3569502-r3)

Reviewer: Variant A (eve-dogma-rs), 2026-10-03 06:05 CST. Method: A implements the same behaviour engine-side
(oracle-checked against Pyfa: bench 1.8.0 cases `aoe_*`, `standup_*`, `incursion_*`). A local build with
`python -m sdepipe build --sde ref/sde/jsonl --out /tmp/ds-proposed --with-proposed` was then run over all 330 bench
and test requests: A's output is **identical** with and without the proposed patches. A keeps these effects
engine-side, so its output does not depend on whether the patches are promoted.

| patch | verdict | notes |
|---|---|---|
| 0101-aoe-burst-projectors | OK to promote, with caveats | Modifiers match Pyfa (PostPercent, target ship / Gunnery turrets / Missile Launcher Operation charges). Caveats: (1) the effects are category 1 (active) and have no `range_attr`. A generic engine that derives a range factor from the effect's range/falloff attributes would get 0 at any distance > 0, but Pyfa applies the bursts at **full strength**. Engines must special-case "no range attribute → factor 1". (2) `structureModuleEffectWeaponDisruption` has range_attr 54 / falloff_attr 2044 (falloffEffectiveness), so the normal range factor applies there. (3) `disallowOffensiveModifiers` must still block them. (4) Neut/ECM bursts are rightly left out. A computes them engine-side as a drain and a jam source. |
| 0102-incursion-system-effects | OK, with one caveat | Targets match Pyfa. Drone scoping: Pyfa hits *all* drones. Every one of the 154 published drones directly requires Drones (3436), so OwnerRequiredSkill(Drones) is equivalent today. Caveat: Pyfa applies all of these **without stacking penalty**, but the patch's resonance modifiers (op 6 on non-stackable attrs, source = a Celestial beacon) are stacking-penalised by the standard dogma rule. A generic engine that also has same-sign resist modifiers (hardeners, other beacons) will differ from Pyfa. Either mark the source as exempt or document the divergence. |
| 0103-breacher-pod-damage-control | OK to promote | Simple active PostPercent on the ship. No bench metric exposes `breacherPodDamageResistance` (only `include_attributes`). |

Other engine-side gaps (from the patches README), status in A:
- Capacitor boosters (`powerBooster`): implemented since v1 (forced reload, capacitorBonus as income; cap sim).
- `doomsdayAOENeut` / `doomsdayAOEECM`: implemented (2026-10-03, bench 1.8.0 cases `aoe_neut_hyperion`, `aoe_ecm_vexor`).
- Weather / AoE cloud beacons: implemented (warfare buffs into the command-bonus pool; drones; penalties per Pyfa),
  bench 1.8.0 cases `weather_*`, `cloud_*`.
