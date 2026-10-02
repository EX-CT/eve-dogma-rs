//! Engine dataset (format v1, produced by `eve-sde-pipeline`).
use rustc_hash::FxHashMap;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Read;

#[derive(Debug, Clone)]
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Func {
    Item,
    Location,
    LocationGroup,
    LocationRequiredSkill,
    OwnerRequiredSkill,
    EffectStopper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy)]
pub struct Modifier {
    pub func: Func,
    pub domain: Domain,
    pub modified: u32,
    pub modifying: u32,
    pub op: i32,
    /// group id (LocationGroup) or skill type id (…RequiredSkill)
    pub extra: u32,
}

#[derive(Debug, Clone)]
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
}

#[derive(Debug, Clone)]
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
}

impl TypeInfo {
    pub fn attr(&self, id: u32) -> Option<f64> {
        self.attrs.iter().find(|(a, _)| *a == id).map(|(_, v)| *v)
    }
    pub fn has_effect(&self, id: u32) -> bool {
        self.effects.iter().any(|(e, _)| *e == id)
    }
}

#[derive(Debug, Clone)]
pub struct GroupInfo {
    pub name: String,
    pub category: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DbuffInfo {
    pub name: Option<String>,
    pub aggregate: Option<String>,
    pub op: i32,
    pub item: Vec<u32>,
    pub location: Vec<u32>,
    pub location_group: Vec<(u32, u32)>,
    pub location_skill: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MutaMapping {
    pub inputs: Vec<u32>,
    pub output: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MutaInfo {
    pub attrs: HashMap<String, (f64, f64)>,
    pub mapping: Vec<MutaMapping>,
}

pub struct Dataset {
    pub build: u64,
    pub release_date: Option<String>,
    pub sha256: String,
    pub types: FxHashMap<u32, TypeInfo>,
    pub groups: FxHashMap<u32, GroupInfo>,
    /// category id -> English name
    pub categories: FxHashMap<u32, String>,
    pub attrs: FxHashMap<u32, AttrInfo>,
    pub effects: FxHashMap<u32, EffectInfo>,
    pub dbuffs: FxHashMap<u32, DbuffInfo>,
    pub mutaplasmids: FxHashMap<u32, MutaInfo>,
    pub names_zh: FxHashMap<u32, String>,
    attr_by_name: FxHashMap<String, u32>,
    effect_by_name: FxHashMap<String, u32>,
    type_by_name: FxHashMap<String, u32>,
    /// all skill type ids (category 16)
    pub skills: Vec<u32>,
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
    pub fn load_path(path: &str) -> Result<Dataset, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
        Self::load_bytes(&bytes)
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
        let mut attrs = FxHashMap::default();
        let mut attr_by_name = FxHashMap::default();
        for (k, a) in raw.attributes {
            let id: u32 = k.parse().unwrap_or(0);
            attr_by_name.insert(a.name.clone(), id);
            attrs.insert(
                id,
                AttrInfo {
                    id,
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
        let mut effects = FxHashMap::default();
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
                },
            );
        }
        let mut groups = FxHashMap::default();
        for (k, g) in raw.groups {
            groups.insert(k.parse().unwrap_or(0), GroupInfo { name: g.name.unwrap_or_default(), category: g.category });
        }
        let categories = raw.categories.into_iter().map(|(k, c)| (k.parse().unwrap_or(0), c.name.unwrap_or_default())).collect();
        let mut types = FxHashMap::default();
        let mut type_by_name = FxHashMap::default();
        let mut skills = Vec::new();
        for (k, t) in raw.types {
            let id: u32 = k.parse().unwrap_or(0);
            let name = t.name.unwrap_or_default();
            if t.published || !type_by_name.contains_key(&name.to_lowercase()) {
                type_by_name.insert(name.to_lowercase(), id);
            }
            if t.category == 16 {
                skills.push(id);
            }
            let mut a: Vec<(u32, f64)> = t.attrs.into_iter().map(|(k, v)| (k.parse().unwrap_or(0), v)).collect();
            a.sort_by_key(|x| x.0);
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
            type_by_name,
            skills,
        })
    }

    pub fn attr_id(&self, name: &str) -> u32 {
        *self.attr_by_name.get(name).unwrap_or(&0)
    }
    pub fn effect_id(&self, name: &str) -> u32 {
        *self.effect_by_name.get(name).unwrap_or(&0)
    }
    pub fn type_by_name(&self, name: &str) -> Option<u32> {
        self.type_by_name.get(&name.trim().to_lowercase()).copied()
    }
    pub fn attr_default(&self, id: u32) -> f64 {
        self.attrs.get(&id).map(|a| a.default).unwrap_or(0.0)
    }
}

// Small self-contained SHA-256 (avoids an extra dependency).
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
        0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
        0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
        0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] =
        [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    let bitlen = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}
