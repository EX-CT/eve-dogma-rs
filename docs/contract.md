# eve-dogma request/response contract (v1, revision 1.4.3)

Stateless: **one JSON `FitRequest` in → one JSON `FitStats` out.** No hidden state, no clocks, no network.
The same request with the same dataset must give byte-identical output. Unknown request fields are ignored.
Breaking changes bump `schema_version` and are listed in the changelog at the end of this file.

## Process interface (CLI)

| Mode | Command | I/O |
|---|---|---|
| single | `eve-dogma calc [FILE]` | FitRequest JSON on stdin (or FILE) → FitStats JSON on stdout, exit 0. On a calc error the `{"error":…}` JSON is still written to stdout and the exit code is 2 |
| batch | `eve-dogma batch` | one FitRequest per line (JSONL) on stdin → one FitStats per line, same order; a failing line yields its `{"error":…}` line in place, exit 0 |
| rpc | `eve-dogma serve-stdio` | JSONL `{"id","method","params"}` → `{"id","result"}`; methods `calc`, `eft_parse` (`{text}`), `eft_export` (`{fit,name?}`), `search` (`{query,limit?,kinds?}`), `type` (`{id}` id or name), `meta` |
| helpers | `eft FILE [--calc] [--skills N]`, `search Q`, `type ID|NAME`, `meta`, `bench FILE -n N` | |

Dataset: `--dataset PATH`, else `$EVE_DOGMA_DATASET`, else `./dataset.json.gz`
(`dataset-<sde_build>.json.gz` from the EX-CT/eve-sde-pipeline releases).

Errors are returned as JSON, never as a panic: `{"error":{"code","message","path"}}`
(codes: `BAD_JSON`, `BAD_REQUEST`, `UNKNOWN_TYPE`, `EFT_PARSE`, `UNKNOWN_METHOD`). Fitting problems are *not*
errors; they are listed in `violations`.
Exit codes: 0 ok · 2 calc/input error (JSON error on stdout for `calc`) · 3 dataset cannot be loaded.

Library (Rust): `eve_dogma::calc(&Dataset, &FitRequest) -> serde_json::Value`, `calc_json(&Dataset, &str) -> String`.

## FitRequest

```jsonc
{
  "schema_version": 1,
  "ship": {"type_id": 587, "mode_type_id": null},            // T3D mode; omitted -> first mode (like the client)
  "character": {
    "skills": {"default_level": 5, "levels": {"3436": 4}},    // every published skill at default_level (0 if omitted), overrides by id or name
    "security_status": null
  },
  "modules": [{
    "type_id": 2889, "slot": "high",                          // slot optional (inferred)
    "state": "active",                                        // offline | online | active | overheated (default online)
    "charge_type_id": 12608,
    "mutation": {"base_type_id": 448, "mutaplasmid_type_id": 47702, "attributes": {"50": 30.0}},  // absolute rolled values by attribute id
    "spool": {"type": "spool_scale", "amount": 1.0}            // spool_scale | cycle_scale | time (s) | cycles
  }],
  "drones":   [{"type_id": 2185, "quantity": 5, "active": 5, "mutation": null}],
  "fighters": [{"type_id": 40556, "quantity": 6, "active": true, "abilities": null}],   // abilities = effect ids; null -> Pyfa defaults
  "implants": [13219],
  "boosters": [{"type_id": 10151, "side_effects": []}],      // side effect ids to apply
  "cargo":    [{"type_id": 32014, "quantity": 10}],
  "fleet": {
    "buffs": [{"buff_id": 10, "value": 25.0}],               // explicit warfare buffs (dbuff id + value)
    "booster_fits": [ /* FitRequest of command ships; strongest value per buff id wins (Pyfa); explicit `buffs` override */ ]
  },
  "projected": [                                             // effects applied TO this fit
    {"kind": "module", "module": {"type_id": 527}, "amount": 2, "distance_m": 5000},
    {"kind": "drone",  "drone":  {"type_id": 23536, "quantity": 2}, "amount": 1, "distance_m": 1000},
    {"kind": "fit",    "fit": { /* FitRequest: computed on its own, active modules/drones projected with its modified values */ }, "amount": 1, "distance_m": 10000}
  ],
  "environment": {"effect_type_ids": [30844], "system_security": "nullsec"},  // hisec | lowsec | nullsec (default) | wspace
  "damage_pattern": {"em": 25, "thermal": 25, "kinetic": 25, "explosive": 25},  // incoming, for EHP/RAH (default uniform)
  "target_profile": {"em": 0, "thermal": 0, "kinetic": 0, "explosive": 0, "signature_radius": 125, "max_velocity": 0, "radius": null},
  "overrides": [{"type_id": 587, "attribute_id": 37, "value": 400}],
  "options": {
    "factor_reload": false, "default_spool": null, "rah": "adapt",   // "disable" = unadapted RAH
    "nos_no_target_cap": false, "include_attributes": null,          // "all" or comma list of attribute names
    "sources": false, "validate": true,
    "cap_sim": {"reload": false, "stagger": false, "max_time_s": null}
  }
}
```

`options` omitted entirely is the same as `"options": {}`: every option takes its default, so **`validate` defaults to
true** either way (violations are reported unless `validate: false` is given).

## Semantics (precise definitions, revisions 1.4.2–1.4.3)

**`capacitor.use_gj_s` / `injected_gj_s` / `delta_gj_s`** (GJ/s, averages, not the simulation):
- `use_gj_s` = Σ over the fit's own modules with state ≥ active and capacitorNeed > 0 of
  `capacitorNeed / avg_cycle_s`, plus Σ over incoming drains (projected neutralizers and enemy nosferatu, every copy)
  of `need / duration_s`.
  - `avg_cycle_s` = (cycle time + reactivation delay) / 1000; with `options.factor_reload` and a finite clip, Pyfa's
    average including one reload per clip: `((cycle + reactivation) × (shots − 1) + (cycle + reload)) / shots`
    (only when the reload is longer than the reactivation delay).
  - Incoming `need` = the source's modified amount × range factor × the target's resistance attribute (if any) ×
    `min(1, signatureRadius / energyNeutralizerSignatureResolution)` when the source has a signature resolution;
    `duration_s` = trunc(duration ms) / 1000.
- `injected_gj_s` = cap *gained* per second, as a positive number:
  - capacitor boosters: `capacitorBonus` of the loaded charge / avg cycle, always reload-inclusive (Pyfa `forceReload`);
  - the fit's own nosferatu: `powerTransferAmount` / avg cycle, counted as income unless `options.nos_no_target_cap`;
  - incoming remote capacitor transmitters: their amount / duration.
- `delta_gj_s = peak_recharge_gj_s + injected_gj_s − use_gj_s`. `stable` / `stable_percent` / `depletes_in_s` come
  from the cap simulation, not from these averages. Drones and fighters never use cap.

**`projected[].amount`** = number of identical, independent sources (default 1):
- `kind: module` / `fighter`: `amount` copies of the module / squadron. For `drone`, there are `amount × quantity` drones.
- `kind: fit`: the source fit is computed **once on its own** (its skills, implants, boosters, fleet; its own
  `projected` list is ignored). Then each of the following is projected `amount` times, all at the entry's `distance_m`:
  - every module with state ≥ active,
  - every active drone (its `active` count),
  - every active fighter squadron.

  Each projected item is frozen with the source-modified attribute values.
- Every copy is a separate modifier: stacking penalties apply across all copies and sources. Remote reps, drains and
  ECM strengths are added per copy. This matches Pyfa's projected amount for modules, drones and fits.

**Fleet buffs** (`fleet.buffs` vs `fleet.booster_fits` vs the fit's own command bursts):
- For each warfare buff id, the candidates are the fit's own active bursts and every active burst on each booster
  fit. Booster fits are computed on their own, without their own booster fits. The single strongest candidate by
  |value| applies (Pyfa).
- **An explicit `fleet.buffs` entry overrides both completely for its buff id.** Bursts and booster fits are ignored
  for that id. Several explicit entries with the same id aggregate to one value: the minimum for buffs whose aggregate
  mode is Minimum, the maximum otherwise. Ids without an explicit entry keep the strongest-candidate rule.
- Environment beacons in `environment.effect_type_ids` whose type carries an abyssal weather (`weather_*`) or AoE
  cloud (`aoe_beacon_*`) effect are candidates too: their `warfareBuff1/2ID` / `Value` join the same per-id
  strongest-|value| pool (the dataset lists them in `environment.effect_beacons[*].dbuffs`). As in Pyfa, buffs
  79, 90, 93–99 also apply to drones that require the Drones skill, and the weather resistance / HP / velocity buffs
  (90, 93–96, 98, 99) are not stacking-penalised.

**Environment system effects without modifiers** (revision 1.4.3): incursion beacons (`OffensiveDefensiveReduction`:
Sansha / Drifter incursion system effects) apply, unpenalised, `systemEffectDamageReduction` (PostPercent) to missile
charges' damage, Smart Bomb damage, turret and drone `damageMultiplier`, and the beacon's armor/shield
`<type>DamageResistanceBonus` to the ship's armor/shield resonances.

**Burst projectors** (`projected[]` modules, revision 1.4.3): the AoE burst projectors (`doomsdayAOE*`, incl. Standup
versions) apply at full strength regardless of `distance_m` (no range factor), are blocked by the target's
`disallowOffensiveModifiers`, and are stacking-penalised. Web / paint / damp / weapon-disruption bursts modify the
target like the single-target modules; the neutralization burst adds a drain (`energyNeutralizerAmount` every
`duration`, × resistance) to the capacitor figures; the ECM burst adds a jam source (`scan<Type>StrengthBonus` ×
resistance) to the jam chance; the warp-disruption burst has no stat effect. The Standup Weapon Disruptor uses the
range factor (maxRange / falloffEffectiveness).

## Search (`search` RPC / CLI), interim

Not part of dogma scoring; the formal spec is deferred to the MCP round. Interim behaviour:
- params `{query, limit?, kinds?}`; `limit` defaults to **20**; `kinds` optionally restricts the categories below.
- Only published types of these kinds: `ship` (cat 6), `module` (7), `charge` (8), `drone` (18), `fighter` (87),
  `implant` / `booster` (20; booster = group name contains "Booster"), `subsystem` (32), `skill` (16).
- Case-insensitive match on the English or Chinese name. Ranking: **exact > prefix > substring**, ties by **typeID
  ascending**. Each result: `{type_id, name, name_zh, group, category_id, kind, slot, meta_level, match: "exact"|"prefix"|"substring"}`.

## EFT export (`eft_export`)

Byte-identical to Pyfa's `service/port/eft.py exportEft` with all options on (implants, boosters, cargo, loaded
charges, mutations), for the fit as Pyfa's GUI holds it (after `fill()`):
- `[Ship, Name]`, a blank line, then sections separated by **two** blank lines; no trailing newline.
- Modules: racks in the order low, med, high, rig, subsystem, service, separated by one blank line; within a rack the
  request order, then `[Empty Low|Med|High|Rig|Subsystem|Service slot]` for every free slot (totals after
  modifiers); `Module, Charge`, then ` /offline`, then ` [N]` for mutated items (base item name is written).
- Drones (`Name xN`, Pyfa DRONE_ORDER by market group, unmutated before mutated, then name) and fighters (`Name xN`,
  light/heavy/support order, N capped at squadron size) form one section, separated by one blank line.
- Implants (by implant slot) and boosters (by booster slot) form one section, separated by one blank line.
- Cargo `Name xN` sorted by (category name, group name, type name).
- Mutation details: `[N] Base`, `  Mutaplasmid`, `  attr value, …` (attribute names sorted, Pyfa `floatUnerr`
  values in Python float repr, e.g. `30.0`).
- **T3D mode (coordinator ruling 2026-10-03 06:14 CST):** follow Pyfa's actual exporter: `eft_export` writes **no**
  mode line (the earlier ruling text "incl. T3D mode line" is superseded). `eft_parse` still accepts a mode line, and
  a missing mode defaults to the first mode. Verified against Pyfa on all bench fits (tests/eft_export_parity.rs).

## FitStats (top level)

`meta` {engine, schema_version, sde_build, dataset_sha256} · `ship` {type_id, name, group} ·
`resources` (cpu/power/calibration/drone_bandwidth/drone_bay/fighter_bay/cargo `{used,total}`, `slots.{high,mid,low,rig,subsystem,service}`,
`hardpoints.{turret,launcher}`, `fighter_tubes.{light,support,heavy,total}`) · `modules[]` (per module: cpu, power, cycle_time_ms, cap_use_gj_s) ·
`offense` (`weapons[]`, `drones[]`, `fighters[]`, `total.{weapon_dps, weapon_volley, drone_dps, drone_volley, fighter_dps, fighter_volley, dps{em,thermal,kinetic,explosive,total}, volley{…}}`, `vs_target_profile`) ·
`defense` (`hp`, `ehp`, `resonance.{shield,armor,hull}.{em,thermal,kinetic,explosive}`, `tank.{raw,effective}.{shield_repair,armor_repair,hull_repair,passive_shield}` HP/s, `damage_pattern`) ·
`capacitor` {capacity, recharge_time_s, peak_recharge_gj_s, use_gj_s, injected_gj_s, delta_gj_s, stable, stable_percent | depletes_in_s, eve_stable_percent, sim_iterations} ·
`navigation` {max_velocity, align_time_s, mass, agility, signature_radius, warp_speed_au_s, max_warp_distance_au, warp_scramble_status} ·
`targeting` {max_targets, max_range_m, scan_resolution, sensor_strength, sensor_type, probe_size, lock_time_s{…}} ·
`drones` {active, max_active, control_range_m} · `violations[]` {code, message, module_index} · `warnings[]` · `attributes` (optional).

Units are in key suffixes (`_m`, `_s`, `_ms`, `_gj_s`, `_au`); resonances are 0..1 (1 = no resist); DPS/HP are per second / absolute.
Violation codes: `CPU_OVERLOAD POWER_OVERLOAD CALIBRATION_OVERLOAD DRONE_BANDWIDTH SLOTS_EXCEEDED TURRET_HARDPOINTS
LAUNCHER_HARDPOINTS SHIP_RESTRICTION RIG_SIZE NOT_FITTABLE MAX_GROUP_FITTED MAX_GROUP_ONLINE MAX_GROUP_ACTIVE MAX_TYPE_FITTED
CHARGE_GROUP CHARGE_SIZE CHARGE_CAPACITY MISSING_SKILL`.

Conventions matching Pyfa (deliberate): volley is spooled; local nosferatu is cap income; missiles use the pilot's
`missileDamageMultiplier`; fighters use Pyfa's default abilities; system security defaults to nullsec.

## Changelog
- v1 (2026-10-03): initial contract.
- v1.1 (2026-10-03): `fleet.booster_fits` implemented (oracle-verified). `projected[kind=fit]` and charges on
  projected modules are still unimplemented (warning only). Non-breaking.
- v1.2 (2026-10-03): `projected[kind=fit]` implemented; charges on projected modules applied (scripts, Nanite Repair
  Paste); incoming remote shield/armor/hull reps add to `defense.tank.raw.*` with Pyfa's diminishing-returns formula;
  incoming neuts/nos/cap transfers are extra capacitor-simulation drains (Pyfa `addDrain`, incl. signature-resolution
  scaling and resistance). Non-breaking (additive). All oracle-verified.
- v1.3 (2026-10-03): `offense.weapons[].range_m` for missiles now follows Pyfa `missileMaxRangeData` (ship-radius
  flight-time bonus, acceleration, floor/ceil blend, FoF limit, centre-to-surface) instead of velocity × flight time.
  Semantic change of one field (not a shape change). Turret optimal/falloff/tracking and missile range/explosion
  radius/velocity are now oracle-verified per weapon (selector pointers `/offense/weapons[module_index=N]/field`).
- v1.4 (2026-10-03): `defense.tank.sustained{,_effective}` (Pyfa `sustainableTank`: when the cap is unstable or reload
  is factored, local cap-using repairers run only as far as peak recharge + injected cap allow). `capacitor.use_gj_s`
  / `injected_gj_s` now include incoming drains/fills, and capacitor boosters' average cycle always includes reload
  (Pyfa `forceReload`). Additive / semantic refinement.
- v1.5 (2026-10-03): `projected[kind=fighter]` (`fighter`: FighterReq; web / warp disruption / neut / ECM abilities,
  Pyfa default abilities unless `abilities` given; projected fits also project their fighters).
  `targeting.jam_chance_percent` (Pyfa jamChance: ECM modules, drones, bursts, fighters vs strongest sensor type).
  `offense.drones[]` gains optimal_m, falloff_m, tracking, max_velocity, signature_radius; `offense.fighters[]` gains
  max_velocity, signature_radius; local fighter MWD / afterburner / evasive maneuvers abilities are applied.
  Booster side effects (`boosters[].side_effects`) oracle-verified. Additive.
- v1.4.1 (2026-10-03 05:30 CST, coordinator rulings; revision label as issued, applies on top of v1.5):
  (1) a calc error exits 2 and still prints the `{"error":…}` JSON on stdout (text fixed; behaviour unchanged);
  (2) `options` missing entirely → `validate` defaults to true (engine fixed: it used to default to false);
  (3) search is out of dogma scope; interim spec above (limit 20, kinds, exact > prefix > substring, typeID ties);
  (4) `eft_export` matches Pyfa's exporter byte for byte (section/blank-line layout, empty-slot lines, ` /offline`
  lowercase, drone/fighter/implant/booster/cargo ordering, mutation block; no T3D mode line because Pyfa writes none);
  (5) duplicate changelog heading removed. Engine speed work (skill pruning, modifier target index) changes no output.
  Ruling 2026-10-03 06:14 CST: the T3D mode line follows Pyfa's actual exporter (none); wording in "EFT export" updated
  (text only, no revision bump, bench frozen at 1.8.0).
- v1.4.2 (2026-10-03 05:45 CST): precise definitions of `capacitor.use_gj_s` / `injected_gj_s` / `delta_gj_s`,
  `projected[].amount` (incl. projected fits: computed once, every active module/drone/fighter projected `amount` times,
  each copy a separate penalised modifier), and fleet-buff precedence (explicit `fleet.buffs` override own bursts and
  booster fits per buff id), see "Semantics". Engine: projected Tracking Disruptors and Guidance Disruptors (Pyfa
  Effect6424 / Effect6423: target's Gunnery modules / Missile Launcher Operation charges, range factor, resistance) are
  now applied (they were a warning before). Oracle-verified, incl. new amount>1 projected-fit cases.
- v1.4.3 (2026-10-03 06:05 CST): no request/response field changes. "Semantics" now also defines abyssal weather /
  AoE cloud environment beacons (warfare buffs in the fleet-buff pool, drone scope, penalties), incursion system
  effects and burst projectors (full strength, no range factor). Doomsday / lance DPS = subcycles × volley / cycle
  (Pyfa `getVolleyParameters`). `cpu_used` / `pg_used` round like Python `round(v, 2)`. Oracle-verified (bench 1.8.0).
