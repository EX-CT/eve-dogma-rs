# eve-dogma-rs design

A stateless, deterministic EVE Online fitting engine. One request (a fit plus character, environment and options) goes in,
one JSON document of fit statistics comes out. Nothing is remembered between requests, so the same dataset and the same
request always give the same bytes. The wire format is described in [docs/contract.md](docs/contract.md).

## Source layout

| file | role |
|---|---|
| `src/data.rs` | dataset model (types, attributes, effects, modifiers, groups, buffs, mutaplasmids), JSON(.gz) loading, binary cache, name indexes |
| `src/request.rs` | `FitRequest` schema (serde) |
| `src/engine.rs` | `Fit`: item graph for one request, modifier registration, lazy attribute evaluation, stacking penalties |
| `src/stats.rs` | derived statistics (resources, offense, defense/EHP/tank, capacitor, navigation, targeting, drones), validation (`violations`) |
| `src/capsim.rs` | event-driven capacitor simulator (behaviour-compatible with Pyfa `capSim.py`) |
| `src/jout.rs` | lightweight output tree `J`: sorted keys, floats rounded to 6 decimals, written straight to text |
| `src/eft.rs` | EFT text import/export (Pyfa byte-exact export) |
| `src/lib.rs` | library API: `calc`, `calc_json`, `request_error_code` |
| `src/main.rs` | CLI (`calc`, `batch`, `serve-stdio`, `eft`, `search`, `type`, `meta`, `bench`) |

## Data flow: dataset -> cache -> calc

1. **Dataset.** `dataset-<build>[-r<rev>].json.gz` comes from EX-CT/eve-sde-pipeline. It holds the SDE dogma tables in
   a normalised form, including each effect's modifier list and the pipeline's named patches (for example
   `0101-aoe-burst-projectors`). The engine has no SDE knowledge of its own beyond that file.
2. **Binary cache.** Parsing about 1 MB of gzip JSON takes tens of milliseconds, so the first load writes a bincode cache
   to `$EVE_DOGMA_CACHE_DIR` (default `<tmp>/eve-dogma-cache`). The cache key is a fast 128-bit hash of the dataset bytes,
   plus the binary's size and mtime and the crate version. A stale entry would need a 128-bit collision, and a failed or
   unreadable cache falls back to parsing the JSON.
3. **Lazy tables.** The cache keeps types and effects as packed records with a dense id -> index table. Each record is
   decoded on first use through a `OnceLock` cell. Names live in separate blobs, so the name indexes (`attr_id`,
   `effect_id`, `type_by_name`, Chinese names) can be built without decoding records. A cold `calc` therefore decodes only
   the ~100 types a fit touches, not all ~50k. A corrupt record panics with a "delete the cache directory" message
   instead of returning wrong numbers.
4. **Calc.** `calc_json` parses the request (`BAD_JSON` / `BAD_REQUEST` on failure), builds a `Fit`, computes the stats
   and writes the `J` tree as compact JSON.

## Modifier pipeline

`Fit::build` creates the items (ship, character, skills, modules, charges, drones, fighters, implants, boosters, modes,
subsystems, projected and fleet sources) and registers every modifier of every active effect:

- **States.** An effect is active if its category fits the item's state (passive/online/active/overheated), as in
  Pyfa/EVE.
- **Targets.** Each modifier's `func` (Item, LocationGroup, LocationRequiredSkill, OwnerRequiredSkill, ...) and `domain`
  (ship, char, target, other, ...) are resolved to target items with a reusable buffer. Skills only register modifiers
  that matter for the fit (`ds.skill_mods()`).
- **Lazy evaluation.** Registration only appends `(op, source item, source attribute, penalised?)` to the target
  attribute. `Fit::get` computes an attribute on first read: base (type value or attribute default) -> operators in dogma
  order (PreAssign, PreMul, PreDiv, ModAdd, ModSub, PostMul, PostDiv, PostPercent, PostAssign) -> `maxAttributeID`
  clamp. The result is memoised. Source values are read recursively with `get`, so the graph is evaluated on demand and
  cycles are not possible in valid data.
- **Engine-side special cases** are kept to things the dataset cannot express: incursion system effects, burst
  projectors / Standup weapon disruptors, Breach Control, reactive armour hardener adaptation, spool-up, overheating
  order and fleet buffs (`dbuffs`). Each guard is mutually exclusive with the dataset's own modifiers. For example,
  effect 4728 (`OffensiveDefensiveReduction`) goes to the engine handler only when the dataset gives it no modifiers or
  does not mark it `stacking_exempt`, so a patch is never applied twice.

## Stacking penalties

A multiplicative modifier (PreMul/PostMul/PostDiv/PostPercent) is penalised when the attribute is not `stackable` and the
source item's category is not exempt: Ship, Charge, Skill, Implant, Subsystem, Structure, or a `stacking_exempt` effect
(treated as category 6). Penalised values are split into positive and negative groups. Each group is sorted by
strength and the i-th is scaled by `exp(-(i/2.67)^2)` (memoised `stack_factor`), as in Pyfa.

## Capacitor simulation

`capsim.rs` is an event-driven port of Pyfa's `capSim.py`:

- drains with cycle times, clip sizes and reloads;
- injectors and staggering (`turretFitted` modules are never staggered);
- incoming neutralisers, nosferatu and remote capacitor transfers;
- an `exp` memo for the recharge curve.

It reports whether the capacitor is stable, the stable percentage or the time to depletion, and the number of
iterations. `py_round1` / `py_round2` / `float_unerr7` reproduce Python rounding exactly where Pyfa rounds.

## Interfaces

- **CLI.** `calc` reads one request from stdin. `batch` runs one request per line, one result per line, for throughput.
  `eft`, `search` (English and Chinese names), `type` and `meta` are also available.
- **JSONL RPC.** `serve-stdio` takes one `{"id","method","params"}` per line. Methods: `calc`, `eft_parse`,
  `eft_export`, `search`, `type`, `meta`.
- **Library.** `eve_dogma::calc(&Dataset, &FitRequest) -> Value` and `calc_json(&Dataset, &str) -> String`.
- **WebAssembly.** The CLI builds for `wasm32-wasip1` (see README). The native-only allocator is left out on wasm32,
  and the same code runs under any WASI runtime (wasmtime, Node's `node:wasi`, or in the browser through a WASI shim).

## Error codes

Errors are JSON, never panics: `{"error":{"code","message","path"}}`.

| code | when |
|---|---|
| `BAD_JSON` | the request text is not valid JSON (syntax error or truncated input) |
| `BAD_REQUEST` | valid JSON that does not match the request schema |
| `UNKNOWN_TYPE` | a type id or name not in the dataset |
| `EFT_PARSE` | EFT text that cannot be parsed |
| `UNKNOWN_METHOD` | RPC method not known |

Fitting problems (CPU/PG/calibration over budget, slots, hardpoints, rig size, max group, charge size/group, missing
skills, ...) are not errors; they are listed in `violations`. Exit codes: 0 ok, 2 input/calc error, 3 dataset cannot be
loaded.

## Determinism and output

Keys are written in sorted order (BTreeMap order) and every float is rounded to 6 decimals. Duplicate names resolve
deterministically (lowest id; for types, published first). Hash maps are never iterated where order would reach the
output. The bench and the Pyfa oracle (`oracle/`) check outputs byte-for-byte; every performance change is checked to
give byte-identical results on the bench corpus, the capacitor sweep, attribute dumps and all ships.

## Performance decisions and trade-offs

- **Lazy, memoised modifier graph.** Only attributes that are read get computed, and each is computed once.
- **Binary cache plus lazy record decode.** About 20% less cold-start work. The cost is about 1-3% warm time from the
  `OnceLock` checks, and cached data is trusted (a corrupt entry panics on use).
- **Output tree `J` instead of `serde_json::Value`.** No BTreeMap or String keys. Frequent literal objects are written
  in key order, so the writer's sort is usually a no-op.
- **Small-vector and reused buffers** on the registration path, pre-sized maps, and skill-attribute capacity hints.
- **Release profile with fat LTO and `codegen-units = 1`.** About 20% lower per-calc wall time and about 14% fewer
  cold-start instructions. The cost is a release build of about 75 s instead of about 20 s. `panic = "abort"` was
  measured but not adopted: it only gave about 2% on cold start and would change the exit behaviour of a panic.
- **mimalloc (v2) allocator** on native targets. Lower per-calc and cold wall time than the v3 default and the system
  allocator, for one C dependency. It is not used on wasm32.
- **Validated name blobs.** The Chinese-name blob is checked as UTF-8 once when its index is built; each lookup only
  checks its own entry (checking the whole blob per lookup made `search` take seconds).
- **No `unsafe`** anywhere in the crate. A few measured shortcuts that needed it were rejected.
