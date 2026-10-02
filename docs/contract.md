# eve-dogma request/response contract (v1)

Stateless: **one JSON `FitRequest` in → one JSON `FitStats` out.** No hidden state, no clocks, no network.
The same request with the same dataset must give byte-identical output. Unknown request fields are ignored.
Breaking changes bump `schema_version` and are listed in the changelog at the end of this file.

## Process interface (CLI)

| Mode | Command | I/O |
|---|---|---|
| single | `eve-dogma calc [FILE]` | FitRequest JSON on stdin (or FILE) → FitStats JSON on stdout, exit 0 |
| batch | `eve-dogma batch` | one FitRequest per line (JSONL) on stdin → one FitStats per line, same order |
| rpc | `eve-dogma serve-stdio` | JSONL `{"id","method","params"}` → `{"id","result"}`; methods `calc`, `eft_parse` (`{text}`), `eft_export` (`{fit,name?}`), `search` (`{query,limit?}`), `type` (`{id}` id or name), `meta` |
| helpers | `eft FILE [--calc] [--skills N]`, `search Q`, `type ID|NAME`, `meta`, `bench FILE -n N` | |

Dataset: `--dataset PATH`, else `$EVE_DOGMA_DATASET`, else `./dataset.json.gz`
(`dataset-<sde_build>.json.gz` from the EX-CT/eve-sde-pipeline releases).

Errors are returned as JSON, never as a panic: `{"error":{"code","message","path"}}`
(codes: `BAD_JSON`, `BAD_REQUEST`, `UNKNOWN_TYPE`, `EFT_PARSE`, `UNKNOWN_METHOD`). Fitting problems are *not*
errors; they are listed in `violations`.

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
    {"kind": "fit",    "fit": { /* FitRequest */  /* NOT YET IMPLEMENTED: emits a warning, no effect */ }, "amount": 1, "distance_m": 10000}
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

## Changelog
- v1.1 (2026-10-03): `fleet.booster_fits` implemented (oracle-verified). `projected[kind=fit]` and charges on
  projected modules are still unimplemented (warning only). Non-breaking.
