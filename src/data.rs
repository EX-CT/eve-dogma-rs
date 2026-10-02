//! Engine dataset (format v1, produced by `eve-sde-pipeline`).
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AttrInfo {
    pub id: u32,
    pub name: String,
    pub default: f64,
    pub stackable: bool,
    pub high_is_good: bool,
    pub min_attr: Option<u32>,
    pub max_attr: Option<u32>,
    pub unit: Option<u32>,
    pub display: Option<String>,
    /// cpu / power / cpuOutput / powerOutput are rounded to 2 decimals (Pyfa)
    pub round2: bool,
    /// `overload*` attribute (read by overheat effects; evaluated in Pyfa's module order)
    #[serde(default)]
    pub overload: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Func {
    Item,
    Location,
    LocationGroup,
    LocationRequiredSkill,
    OwnerRequiredSkill,
    EffectStopper,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    Item,
    Ship,
    Char,
    Other,
    Structure,
    TargetId,
    Target,
    None,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct Modifier {
    pub func: Func,
    pub domain: Domain,
    pub modified: u32,
    pub modifying: u32,
    pub op: i32,
    /// group id (LocationGroup) or skill type id (…RequiredSkill)
    pub extra: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EffectInfo {
    pub id: u32,
    pub name: String,
    pub category: u8,
    pub duration_attr: Option<u32>,
    pub discharge_attr: Option<u32>,
    pub range_attr: Option<u32>,
    pub falloff_attr: Option<u32>,
    pub tracking_attr: Option<u32>,
    pub resistance_attr: Option<u32>,
    pub fitting_usage_chance_attr: Option<u32>,
    pub is_offensive: bool,
    pub is_assistance: bool,
    pub mods: Vec<Modifier>,
    /// dataset flag (revision 4+): modifiers of this effect are never stacking-penalised
    #[serde(default)]
    pub stacking_exempt: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TypeInfo {
    pub id: u32,
    pub name: String,
    pub group: u32,
    pub category: u32,
    pub published: bool,
    pub mass: f64,
    pub volume: f64,
    pub capacity: f64,
    pub radius: f64,
    pub market_group: Option<u32>,
    pub meta_group: Option<u32>,
    pub meta_level: Option<i32>,
    pub variation_parent: Option<u32>,
    pub attrs: Vec<(u32, f64)>,
    pub effects: Vec<(u32, bool)>,
    /// non-zero requiredSkill1..6 values (computed at load)
    pub req_skills: Vec<u32>,
}

impl TypeInfo {
    pub fn attr(&self, id: u32) -> Option<f64> {
        // attrs are sorted by id at load
        self.attrs.binary_search_by_key(&id, |x| x.0).ok().map(|i| self.attrs[i].1)
    }
    pub fn has_effect(&self, id: u32) -> bool {
        self.effects.iter().any(|(e, _)| *e == id)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GroupInfo {
    pub name: String,
    pub category: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbuffInfo {
    pub name: Option<String>,
    pub aggregate: Option<String>,
    pub op: i32,
    pub item: Vec<u32>,
    pub location: Vec<u32>,
    pub location_group: Vec<(u32, u32)>,
    pub location_skill: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutaMapping {
    pub inputs: Vec<u32>,
    pub output: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutaInfo {
    pub attrs: HashMap<String, (f64, f64)>,
    pub mapping: Vec<MutaMapping>,
}

/// id-indexed table (attribute / effect ids are small and dense): O(1) lookups without hashing
#[derive(Serialize, Deserialize)]
pub struct Dense<T> {
    v: Vec<Option<T>>,
    n: usize,
}

impl<T> Default for Dense<T> {
    fn default() -> Self {
        Dense { v: Vec::new(), n: 0 }
    }
}

impl<T> Dense<T> {
    #[inline]
    pub fn get(&self, id: &u32) -> Option<&T> {
        self.v.get(*id as usize).and_then(|x| x.as_ref())
    }
    pub fn insert(&mut self, id: u32, t: T) {
        let i = id as usize;
        if i >= self.v.len() {
            self.v.resize_with(i + 1, || None);
        }
        if self.v[i].replace(t).is_none() {
            self.n += 1;
        }
    }
    pub fn len(&self) -> usize {
        self.n
    }
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }
    pub fn contains_key(&self, id: &u32) -> bool {
        self.get(id).is_some()
    }
    pub fn iter(&self) -> impl Iterator<Item = (u32, &T)> {
        self.v.iter().enumerate().filter_map(|(i, x)| x.as_ref().map(|t| (i as u32, t)))
    }
}

#[derive(Serialize, Deserialize)]
pub struct Dataset {
    pub build: u64,
    pub release_date: Option<String>,
    pub sha256: String,
    pub types: FxHashMap<u32, TypeInfo>,
    pub groups: FxHashMap<u32, GroupInfo>,
    /// category id -> English name
    pub categories: FxHashMap<u32, String>,
    pub attrs: Dense<AttrInfo>,
    pub effects: Dense<EffectInfo>,
    pub dbuffs: FxHashMap<u32, DbuffInfo>,
    pub mutaplasmids: FxHashMap<u32, MutaInfo>,
    pub names_zh: FxHashMap<u32, String>,
    attr_by_name: FxHashMap<String, u32>,
    effect_by_name: FxHashMap<String, u32>,
    /// lowercase name -> type id, built on first use (not stored in the binary cache: most calcs never need it).
    /// A published type wins over unpublished ones of the same name; otherwise the lowest id.
    #[serde(skip)]
    type_by_name: std::sync::OnceLock<FxHashMap<String, u32>>,
    /// all skill type ids (category 16)
    pub skills: Vec<u32>,
    /// attribute ids looked up by name on hot paths, resolved once at load
    pub wk: WellKnown,
}

#[derive(Default, Serialize, Deserialize)]
pub struct WellKnown {
    pub can_fit_group: Vec<u32>,
    pub can_fit_type: Vec<u32>,
    pub charge_group: Vec<u32>,
    /// (requiredSkillN, requiredSkillNLevel)
    pub req_skill: Vec<(u32, u32)>,
    /// published skill type ids, ascending
    pub published_skills: Vec<u32>,
    /// tactical destroyer modes (group 1306): (type id, lowercase name), ascending by id
    pub mode_types: Vec<(u32, String)>,
}

// ---------- raw serde shapes ----------
#[derive(Deserialize)]
struct RawDs {
    format: String,
    format_version: u32,
    sde: RawSde,
    groups: HashMap<String, RawGroup>,
    #[serde(default)]
    categories: HashMap<String, RawCategory>,
    attributes: HashMap<String, RawAttr>,
    effects: HashMap<String, RawEffect>,
    types: HashMap<String, RawType>,
    #[serde(default)]
    dbuffs: HashMap<String, DbuffInfo>,
    #[serde(default)]
    mutaplasmids: HashMap<String, MutaInfo>,
    #[serde(default)]
    names: HashMap<String, HashMap<String, String>>,
}
#[derive(Deserialize)]
struct RawSde {
    build: u64,
    release_date: Option<String>,
}
#[derive(Deserialize)]
struct RawCategory {
    #[serde(default)]
    name: Option<String>,
}
#[derive(Deserialize)]
struct RawGroup {
    name: Option<String>,
    category: u32,
}
#[derive(Deserialize)]
struct RawAttr {
    name: String,
    #[serde(default)]
    default: f64,
    #[serde(default = "t")]
    stackable: bool,
    #[serde(default = "t")]
    high_is_good: bool,
    min_attr: Option<u32>,
    max_attr: Option<u32>,
    unit: Option<u32>,
    display: Option<String>,
}
fn t() -> bool {
    true
}
#[derive(Deserialize)]
struct RawEffect {
    name: String,
    #[serde(default)]
    category: u8,
    duration_attr: Option<u32>,
    discharge_attr: Option<u32>,
    range_attr: Option<u32>,
    falloff_attr: Option<u32>,
    tracking_attr: Option<u32>,
    resistance_attr: Option<u32>,
    fitting_usage_chance_attr: Option<u32>,
    #[serde(default)]
    is_offensive: bool,
    #[serde(default)]
    is_assistance: bool,
    #[serde(default)]
    mods: Vec<(i32, i32, u32, u32, i32, u32)>,
    #[serde(default)]
    stacking_exempt: bool,
}
#[derive(Deserialize)]
struct RawType {
    name: Option<String>,
    group: u32,
    category: u32,
    #[serde(default)]
    published: bool,
    #[serde(default)]
    mass: f64,
    #[serde(default)]
    volume: f64,
    #[serde(default)]
    capacity: f64,
    #[serde(default)]
    radius: f64,
    market_group: Option<u32>,
    meta_group: Option<u32>,
    meta_level: Option<i32>,
    variation_parent: Option<u32>,
    #[serde(default)]
    attrs: HashMap<String, f64>,
    #[serde(default)]
    effects: Vec<(u32, u8)>,
}

fn func_of(c: i32) -> Func {
    match c {
        0 => Func::Item,
        1 => Func::Location,
        2 => Func::LocationGroup,
        3 => Func::LocationRequiredSkill,
        4 => Func::OwnerRequiredSkill,
        _ => Func::EffectStopper,
    }
}
fn domain_of(c: i32) -> Domain {
    match c {
        0 => Domain::Item,
        1 => Domain::Ship,
        2 => Domain::Char,
        3 => Domain::Other,
        4 => Domain::Structure,
        5 => Domain::TargetId,
        6 => Domain::Target,
        _ => Domain::None,
    }
}

impl Dataset {
    /// Load a dataset file. A binary cache of the parsed dataset (bincode) is kept in `$EVE_DOGMA_CACHE_DIR`
    /// (default: `<tmp>/eve-dogma-cache`), keyed by the SHA-256 of the file bytes and by this executable's size and
    /// mtime, so a rebuilt engine or a changed dataset never reads a stale cache. `EVE_DOGMA_NO_CACHE=1` disables it.
    /// Results are identical with or without the cache (the cache holds the fully parsed `Dataset`).
    pub fn load_path(path: &str) -> Result<Dataset, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
        let cache = if std::env::var_os("EVE_DOGMA_NO_CACHE").is_some() { None } else { cache_file(&bytes) };
        if let Some(cf) = &cache {
            if let Ok(b) = std::fs::read(cf) {
                if let Ok(ds) = bincode::deserialize::<Dataset>(&b) {
                    return Ok(ds);
                }
            }
        }
        let ds = Self::load_bytes(&bytes)?;
        if let Some(cf) = cache {
            if let Ok(b) = bincode::serialize(&ds) {
                if let Some(dir) = cf.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let tmp = cf.with_extension(format!("tmp{}", std::process::id()));
                if std::fs::write(&tmp, &b).is_ok() && std::fs::rename(&tmp, &cf).is_err() {
                    let _ = std::fs::remove_file(&tmp);
                }
                if let Some(dir) = cf.parent() {
                    prune_cache(dir);
                }
            }
        }
        Ok(ds)
    }

    pub fn load_bytes(bytes: &[u8]) -> Result<Dataset, String> {
        let json: Vec<u8> = if bytes.len() > 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
            let mut d = flate2::read::GzDecoder::new(bytes);
            let mut out = Vec::with_capacity(bytes.len() * 12);
            d.read_to_end(&mut out).map_err(|e| format!("gunzip: {e}"))?;
            out
        } else {
            bytes.to_vec()
        };
        let sha256 = sha256_hex(&json);
        let raw: RawDs = serde_json::from_slice(&json).map_err(|e| format!("dataset json: {e}"))?;
        if raw.format != "exct-eve-dataset" || raw.format_version != 1 {
            return Err(format!("unsupported dataset format {} v{}", raw.format, raw.format_version));
        }
        let mut attrs = Dense::default();
        let mut attr_by_name = FxHashMap::default();
        for (k, a) in raw.attributes {
            let id: u32 = k.parse().unwrap_or(0);
            attr_by_name.insert(a.name.clone(), id);
            attrs.insert(
                id,
                AttrInfo {
                    id,
                    round2: matches!(a.name.as_str(), "cpu" | "power" | "cpuOutput" | "powerOutput"),
                    overload: a.name.starts_with("overload"),
                    name: a.name,
                    default: a.default,
                    stackable: a.stackable,
                    high_is_good: a.high_is_good,
                    min_attr: a.min_attr,
                    max_attr: a.max_attr,
                    unit: a.unit,
                    display: a.display,
                },
            );
        }
        let mut effects = Dense::default();
        let mut effect_by_name = FxHashMap::default();
        for (k, e) in raw.effects {
            let id: u32 = k.parse().unwrap_or(0);
            effect_by_name.insert(e.name.clone(), id);
            let mods = e
                .mods
                .iter()
                .map(|&(f, d, modified, modifying, op, extra)| Modifier {
                    func: func_of(f),
                    domain: domain_of(d),
                    modified,
                    modifying,
                    op,
                    extra,
                })
                .collect();
            effects.insert(
                id,
                EffectInfo {
                    id,
                    name: e.name,
                    category: e.category,
                    duration_attr: e.duration_attr,
                    discharge_attr: e.discharge_attr,
                    range_attr: e.range_attr,
                    falloff_attr: e.falloff_attr,
                    tracking_attr: e.tracking_attr,
                    resistance_attr: e.resistance_attr,
                    fitting_usage_chance_attr: e.fitting_usage_chance_attr,
                    is_offensive: e.is_offensive,
                    is_assistance: e.is_assistance,
                    mods,
                    stacking_exempt: e.stacking_exempt,
                },
            );
        }
        let mut groups = FxHashMap::default();
        for (k, g) in raw.groups {
            groups.insert(k.parse().unwrap_or(0), GroupInfo { name: g.name.unwrap_or_default(), category: g.category });
        }
        let categories = raw.categories.into_iter().map(|(k, c)| (k.parse().unwrap_or(0), c.name.unwrap_or_default())).collect();
        let mut types = FxHashMap::default();
        let mut skills = Vec::new();
        for (k, t) in raw.types {
            let id: u32 = k.parse().unwrap_or(0);
            let name = t.name.unwrap_or_default();
            if t.category == 16 {
                skills.push(id);
            }
            let mut a: Vec<(u32, f64)> = t.attrs.into_iter().map(|(k, v)| (k.parse().unwrap_or(0), v)).collect();
            a.sort_by_key(|x| x.0);
            // type-level fields are authoritative for mass/capacity/volume/radius (present even when 0)
            for (aid, v) in [(4u32, t.mass), (38, t.capacity), (161, t.volume), (162, t.radius)] {
                match a.binary_search_by_key(&aid, |x| x.0) {
                    Ok(i) => {
                        if v != 0.0 {
                            a[i].1 = v
                        }
                    }
                    Err(i) => a.insert(i, (aid, v)),
                }
            }
            types.insert(
                id,
                TypeInfo {
                    id,
                    name,
                    group: t.group,
                    category: t.category,
                    published: t.published,
                    mass: t.mass,
                    volume: t.volume,
                    capacity: t.capacity,
                    radius: t.radius,
                    market_group: t.market_group,
                    meta_group: t.meta_group,
                    meta_level: t.meta_level,
                    variation_parent: t.variation_parent,
                    req_skills: [182u32, 183, 184, 1285, 1289, 1290]
                        .iter()
                        .filter_map(|id| a.binary_search_by_key(id, |x| x.0).ok().map(|i| a[i].1 as u32))
                        .filter(|v| *v != 0)
                        .collect(),
                    attrs: a,
                    effects: t.effects.into_iter().map(|(e, d)| (e, d != 0)).collect(),
                },
            );
        }
        skills.sort();
        let dbuffs = raw.dbuffs.into_iter().map(|(k, v)| (k.parse().unwrap_or(0), v)).collect();
        let mutaplasmids = raw.mutaplasmids.into_iter().map(|(k, v)| (k.parse().unwrap_or(0), v)).collect();
        let names_zh = raw
            .names
            .get("zh")
            .map(|m| m.iter().map(|(k, v)| (k.parse().unwrap_or(0), v.clone())).collect())
            .unwrap_or_default();
        Ok(Dataset {
            build: raw.sde.build,
            release_date: raw.sde.release_date,
            sha256,
            types,
            groups,
            categories,
            attrs,
            effects,
            dbuffs,
            mutaplasmids,
            names_zh,
            attr_by_name,
            effect_by_name,
            type_by_name: std::sync::OnceLock::new(),
            skills,
            wk: WellKnown::default(),
        })
        .map(|mut d: Dataset| {
            let a = |n: &str| d.attr_id(n);
            d.wk = WellKnown {
                can_fit_group: (1..=20).map(|k| a(&format!("canFitShipGroup{k:02}"))).filter(|x| *x != 0).collect(),
                can_fit_type: (1..=11).map(|k| a(&format!("canFitShipType{k}"))).filter(|x| *x != 0).collect(),
                charge_group: (1..=5).map(|k| a(&format!("chargeGroup{k}"))).filter(|x| *x != 0).collect(),
                req_skill: (1..=6).map(|k| (a(&format!("requiredSkill{k}")), a(&format!("requiredSkill{k}Level")))).filter(|x| x.0 != 0).collect(),
                published_skills: {
                    let mut v: Vec<u32> = d.skills.iter().copied().filter(|s| d.types.get(s).map(|t| t.published).unwrap_or(false)).collect();
                    v.sort_unstable();
                    v.dedup();
                    v
                },
                mode_types: {
                    let mut v: Vec<(u32, String)> = d.types.iter().filter(|(_, t)| t.group == 1306).map(|(id, t)| (*id, t.name.to_lowercase())).collect();
                    v.sort_unstable();
                    v
                },
            };
            d
        })
    }

    pub fn attr_id(&self, name: &str) -> u32 {
        *self.attr_by_name.get(name).unwrap_or(&0)
    }
    pub fn effect_id(&self, name: &str) -> u32 {
        *self.effect_by_name.get(name).unwrap_or(&0)
    }
    pub fn type_by_name(&self, name: &str) -> Option<u32> {
        self.type_by_name
            .get_or_init(|| {
                let mut ids: Vec<(&u32, &TypeInfo)> = self.types.iter().collect();
                ids.sort_unstable_by_key(|(id, _)| **id);
                let mut m: FxHashMap<String, u32> = FxHashMap::with_capacity_and_hasher(ids.len(), Default::default());
                for (id, t) in ids {
                    let k = t.name.to_lowercase();
                    match m.get(&k) {
                        Some(prev) if !(t.published && !self.types[prev].published) => {}
                        _ => {
                            m.insert(k, *id);
                        }
                    }
                }
                m
            })
            .get(&name.trim().to_lowercase())
            .copied()
    }
    pub fn attr_default(&self, id: u32) -> f64 {
        self.attrs.get(&id).map(|a| a.default).unwrap_or(0.0)
    }
}

// Small self-contained SHA-256 (avoids an extra dependency).
fn cache_file(bytes: &[u8]) -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let m = std::fs::metadata(&exe).ok()?;
    let mtime = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    let dir = std::env::var_os("EVE_DOGMA_CACHE_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("eve-dogma-cache"));
    // content key: a fast 128-bit non-cryptographic hash of the dataset file (the sha256 of a 0.9 MB file was ~40% of
    // a cached cold start without SHA CPU extensions); a stale entry can only come from a 128-bit collision
    let (h1, h2) = fast_hash128(bytes);
    let key = sha256_hex(format!("{h1:016x}{h2:016x}|{}|{}|{}|{}", bytes.len(), m.len(), mtime, env!("CARGO_PKG_VERSION")).as_bytes());
    Some(dir.join(format!("ds-{}.bin", &key[..32])))
}

/// two independent 64-bit multiply-xorshift lanes over 8-byte words (+ the tail)
fn fast_hash128(b: &[u8]) -> (u64, u64) {
    let (mut h1, mut h2) = (0x9e37_79b9_7f4a_7c15u64 ^ b.len() as u64, 0xc2b2_ae3d_27d4_eb4fu64);
    let mut ch = b.chunks_exact(8);
    for c in &mut ch {
        let w = u64::from_le_bytes(c.try_into().unwrap());
        h1 = (h1.rotate_left(5) ^ w).wrapping_mul(0x51_7cc1_b727_220a_95);
        h2 = (h2 ^ w.rotate_left(29)).wrapping_mul(0x9fb2_1c65_1e98_df25).rotate_left(31);
    }
    for &x in ch.remainder() {
        h1 = (h1.rotate_left(5) ^ x as u64).wrapping_mul(0x51_7cc1_b727_220a_95);
        h2 = (h2 ^ x as u64).wrapping_mul(0x9fb2_1c65_1e98_df25).rotate_left(31);
    }
    let fin = |mut h: u64| {
        h ^= h >> 33;
        h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
        h ^= h >> 33;
        h
    };
    (fin(h1), fin(h2 ^ h1.rotate_left(17)))
}

/// keep the cache directory small: after writing a new entry, remove all but the 6 most recent ds-*.bin files
fn prune_cache(dir: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut v: Vec<(std::time::SystemTime, std::path::PathBuf)> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("ds-") && e.file_name().to_string_lossy().ends_with(".bin"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, p) in v.into_iter().skip(6) {
        let _ = std::fs::remove_file(p);
    }
}

pub fn sha256_hex(data: &[u8]) -> String {
    // sha2 uses the CPU's SHA extensions when present (runtime detection); the dataset hash is ~9 MB per load
    use sha2::{Digest, Sha256};
    let d = Sha256::digest(data);
    d.iter().map(|b| format!("{b:02x}")).collect()
}
