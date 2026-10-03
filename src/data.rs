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

/// (de)serialized as one packed byte record (see `TypeInfo::pack`): a cached cold start decodes ~10k types
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

/// Attribute / effect table for the binary cache: each entry is its own bincode record, decoded on first lookup,
/// with the names kept in one separate blob (for the name indexes) so a cached cold start decodes only what it uses.
pub struct Table<T> {
    /// ids, ascending
    ids: Vec<u32>,
    pos: TypePos,
    raw: Vec<u8>,
    offs: Vec<u32>,
    names: String,
    name_offs: Vec<u32>,
    cells: Box<[std::sync::OnceLock<T>]>,
}

fn type_pos(ids: &[u32]) -> TypePos {
    let max = ids.last().copied().unwrap_or(0) as usize;
    if max <= 1 << 22 {
        let mut d = vec![0u32; max + 1];
        for (i, &id) in ids.iter().enumerate() {
            d[id as usize] = i as u32 + 1;
        }
        TypePos::Dense(d)
    } else {
        TypePos::Map(ids.iter().enumerate().map(|(i, &id)| (id, i as u32)).collect())
    }
}

impl<T: Serialize + for<'de> Deserialize<'de>> Table<T> {
    fn from_dense(d: Dense<T>, name: impl Fn(&T) -> &str) -> Table<T> {
        let (mut ids, mut raw, mut offs, mut names, mut name_offs, mut cells) = (Vec::new(), Vec::new(), vec![0u32], String::new(), vec![0u32], Vec::new());
        for (i, x) in d.v.into_iter().enumerate() {
            let Some(t) = x else { continue };
            ids.push(i as u32);
            raw.extend_from_slice(&bincode::serialize(&t).expect("serialize table entry"));
            offs.push(raw.len() as u32);
            names.push_str(name(&t));
            name_offs.push(names.len() as u32);
            cells.push(std::sync::OnceLock::from(t));
        }
        let pos = type_pos(&ids);
        Table { ids, pos, raw, offs, names, name_offs, cells: cells.into_boxed_slice() }
    }

    #[inline]
    fn index(&self, id: u32) -> Option<usize> {
        match &self.pos {
            TypePos::Dense(d) => match d.get(id as usize) {
                Some(&k) if k > 0 => Some(k as usize - 1),
                _ => None,
            },
            TypePos::Map(m) => m.get(&id).map(|&k| k as usize),
        }
    }

    fn at(&self, i: usize) -> &T {
        self.cells[i].get_or_init(|| {
            let rec = &self.raw[self.offs[i] as usize..self.offs[i + 1] as usize];
            match bincode::deserialize::<T>(rec) {
                Ok(t) => t,
                Err(_) => panic!("corrupt dataset cache entry {} (delete the eve-dogma cache directory)", self.ids[i]),
            }
        })
    }

    #[inline]
    pub fn get(&self, id: &u32) -> Option<&T> {
        self.index(*id).map(|i| self.at(i))
    }

    pub fn contains_key(&self, id: &u32) -> bool {
        self.index(*id).is_some()
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// all entries in ascending id order (decodes every record)
    pub fn iter(&self) -> impl Iterator<Item = (u32, &T)> {
        self.ids.iter().enumerate().map(move |(i, &id)| (id, self.at(i)))
    }

    fn name_at(&self, i: usize) -> &str {
        self.names.get(self.name_offs[i] as usize..self.name_offs[i + 1] as usize).unwrap_or("")
    }

    /// name of entry `id` without decoding it
    pub fn name(&self, id: u32) -> Option<&str> {
        self.index(id).map(|i| self.name_at(i))
    }

    /// (id, name) in ascending id order, without decoding entries
    pub fn names(&self) -> impl Iterator<Item = (u32, &str)> {
        self.ids.iter().enumerate().map(move |(i, &id)| (id, self.name_at(i)))
    }
}

fn u32s_bytes(v: &[u32]) -> RawBytes {
    RawBytes(v.iter().flat_map(|x| x.to_le_bytes()).collect())
}

fn bytes_u32s(b: &[u8]) -> Option<Vec<u32>> {
    if b.len() % 4 != 0 {
        return None;
    }
    Some(b.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}

impl<T> Serialize for Table<T> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        (u32s_bytes(&self.ids), u32s_bytes(&self.offs), RawBytes(self.raw.clone()), &self.names, u32s_bytes(&self.name_offs)).serialize(s)
    }
}

impl<'de, T> Deserialize<'de> for Table<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let (ids, offs, raw, names, name_offs) = <(RawBytes, RawBytes, RawBytes, String, RawBytes)>::deserialize(d)?;
        let bad = || D::Error::custom("bad table");
        let (ids, offs, raw, name_offs) = (bytes_u32s(&ids.0).ok_or_else(bad)?, bytes_u32s(&offs.0).ok_or_else(bad)?, raw.0, bytes_u32s(&name_offs.0).ok_or_else(bad)?);
        let ok = |o: &[u32], len: usize| o.len() == ids.len() + 1 && o.first() == Some(&0) && o.last().map(|&x| x as usize) == Some(len) && o.windows(2).all(|w| w[0] <= w[1]);
        if !ok(&offs, raw.len()) || !ok(&name_offs, names.len()) || ids.windows(2).any(|w| w[0] >= w[1]) {
            return Err(bad());
        }
        let cells = (0..ids.len()).map(|_| std::sync::OnceLock::new()).collect();
        let pos = type_pos(&ids);
        Ok(Table { ids, pos, raw, offs, names, name_offs, cells })
    }
}

/// name -> id without owning the names: 64-bit name hash -> id, the name checked against the owner on lookup; ids
/// whose different name collides on the hash go to `extra` (scanned linearly)
#[derive(Default)]
struct NameIndex {
    map: FxHashMap<u64, u32>,
    extra: Vec<u32>,
}

impl NameIndex {
    fn hash(s: &str) -> u64 {
        use std::hash::Hasher;
        let mut h = rustc_hash::FxHasher::default();
        h.write(s.as_bytes());
        h.write_usize(s.len());
        h.finish()
    }

    /// `items` in ascending id order (the lowest id wins for a duplicated name); `names` looks a name up by id
    fn build<'a>(items: impl Iterator<Item = (u32, &'a str)>, names: impl Fn(u32) -> Option<&'a str>) -> NameIndex {
        let mut ix = NameIndex { map: FxHashMap::with_capacity_and_hasher(items.size_hint().0.max(4096), Default::default()), extra: Vec::new() };
        for (id, name) in items {
            let h = NameIndex::hash(name);
            match ix.map.get(&h) {
                None => {
                    ix.map.insert(h, id);
                }
                Some(&prev) if names(prev) == Some(name) => {}
                Some(_) => {
                    if !ix.extra.iter().any(|&x| names(x) == Some(name)) {
                        ix.extra.push(id);
                    }
                }
            }
        }
        ix
    }

    fn find<'a>(&self, name: &str, names: impl Fn(u32) -> Option<&'a str>) -> u32 {
        if let Some(&id) = self.map.get(&NameIndex::hash(name)) {
            if names(id) == Some(name) {
                return id;
            }
        }
        self.extra.iter().copied().find(|&id| names(id) == Some(name)).unwrap_or(0)
    }
}

/// packed little-endian record of a TypeInfo: fixed scalars, optional ids (flag byte + value), then the name and
/// the attribute / effect / required-skill lists each as a u32 count followed by their elements
impl TypeInfo {
    fn pack(&self, o: &mut Vec<u8>) {
        fn opt(o: &mut Vec<u8>, v: Option<u32>) {
            match v {
                Some(x) => {
                    o.push(1);
                    o.extend_from_slice(&x.to_le_bytes())
                }
                None => o.push(0),
            }
        }
        for x in [self.id, self.group, self.category] {
            o.extend_from_slice(&x.to_le_bytes());
        }
        o.push(self.published as u8);
        for x in [self.mass, self.volume, self.capacity, self.radius] {
            o.extend_from_slice(&x.to_le_bytes());
        }
        opt(o, self.market_group);
        opt(o, self.meta_group);
        opt(o, self.meta_level.map(|x| x as u32));
        opt(o, self.variation_parent);
        o.extend_from_slice(&(self.name.len() as u32).to_le_bytes());
        o.extend_from_slice(self.name.as_bytes());
        o.extend_from_slice(&(self.attrs.len() as u32).to_le_bytes());
        for (a, v) in &self.attrs {
            o.extend_from_slice(&a.to_le_bytes());
            o.extend_from_slice(&v.to_le_bytes());
        }
        o.extend_from_slice(&(self.effects.len() as u32).to_le_bytes());
        for (e, d) in &self.effects {
            o.extend_from_slice(&e.to_le_bytes());
            o.push(*d as u8);
        }
        o.extend_from_slice(&(self.req_skills.len() as u32).to_le_bytes());
        for s in &self.req_skills {
            o.extend_from_slice(&s.to_le_bytes());
        }
    }

    fn unpack(b: &[u8]) -> Option<TypeInfo> {
        struct R<'a>(&'a [u8]);
        impl<'a> R<'a> {
            #[inline]
            fn take(&mut self, n: usize) -> Option<&'a [u8]> {
                if self.0.len() < n {
                    return None;
                }
                let (a, b) = self.0.split_at(n);
                self.0 = b;
                Some(a)
            }
            #[inline]
            fn u8(&mut self) -> Option<u8> {
                Some(self.take(1)?[0])
            }
            #[inline]
            fn u32(&mut self) -> Option<u32> {
                Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
            }
            #[inline]
            fn f64(&mut self) -> Option<f64> {
                Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
            }
            #[inline]
            fn opt(&mut self) -> Option<Option<u32>> {
                Some(if self.u8()? != 0 { Some(self.u32()?) } else { None })
            }
        }
        let mut r = R(b);
        let (id, group, category) = (r.u32()?, r.u32()?, r.u32()?);
        let published = r.u8()? != 0;
        let (mass, volume, capacity, radius) = (r.f64()?, r.f64()?, r.f64()?, r.f64()?);
        let (market_group, meta_group, meta_level, variation_parent) = (r.opt()?, r.opt()?, r.opt()?.map(|x| x as i32), r.opt()?);
        let n = r.u32()? as usize;
        let name = std::str::from_utf8(r.take(n)?).ok()?.to_string();
        let n = r.u32()? as usize;
        let attrs = r
            .take(n.checked_mul(12)?)?
            .chunks_exact(12)
            .map(|c| (u32::from_le_bytes([c[0], c[1], c[2], c[3]]), f64::from_le_bytes([c[4], c[5], c[6], c[7], c[8], c[9], c[10], c[11]])))
            .collect();
        let n = r.u32()? as usize;
        let effects = r.take(n.checked_mul(5)?)?.chunks_exact(5).map(|c| (u32::from_le_bytes([c[0], c[1], c[2], c[3]]), c[4] != 0)).collect();
        let n = r.u32()? as usize;
        let req_skills = r.take(n.checked_mul(4)?)?.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        if !r.0.is_empty() {
            return None;
        }
        Some(TypeInfo { id, name, group, category, published, mass, volume, capacity, radius, market_group, meta_group, meta_level, variation_parent, attrs, effects, req_skills })
    }
}

impl Serialize for TypeInfo {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut o = Vec::with_capacity(96 + self.name.len() + 12 * self.attrs.len());
        self.pack(&mut o);
        s.serialize_bytes(&o)
    }
}

impl<'de> Deserialize<'de> for TypeInfo {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = TypeInfo;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("packed TypeInfo")
            }
            fn visit_bytes<E: serde::de::Error>(self, b: &[u8]) -> Result<TypeInfo, E> {
                TypeInfo::unpack(b).ok_or_else(|| E::custom("bad packed TypeInfo"))
            }
            fn visit_borrowed_bytes<E: serde::de::Error>(self, b: &'de [u8]) -> Result<TypeInfo, E> {
                self.visit_bytes(b)
            }
            fn visit_byte_buf<E: serde::de::Error>(self, b: Vec<u8>) -> Result<TypeInfo, E> {
                self.visit_bytes(&b)
            }
        }
        d.deserialize_bytes(V)
    }
}

/// All types of the dataset. In the binary cache each type is one packed record (`TypeInfo::pack`); a record is
/// decoded the first time the type is looked up, so a cached cold start does not decode ~10k types to use ~100.
pub struct Types {
    /// type ids, ascending
    ids: Vec<u32>,
    pos: TypePos,
    raw: Vec<u8>,
    /// record i is raw[offs[i]..offs[i + 1]]
    offs: Vec<u32>,
    cells: Box<[std::sync::OnceLock<Box<TypeInfo>>]>,
}

/// type id -> index into `ids`: a dense table (stored index + 1, 0 = absent) while ids are small, else a map
enum TypePos {
    Dense(Vec<u32>),
    Map(FxHashMap<u32, u32>),
}

impl Types {
    fn build(ids: Vec<u32>, raw: Vec<u8>, offs: Vec<u32>, cells: Box<[std::sync::OnceLock<Box<TypeInfo>>]>) -> Types {
        let max = ids.last().copied().unwrap_or(0) as usize;
        let pos = if max <= 1 << 22 {
            let mut d = vec![0u32; max + 1];
            for (i, &id) in ids.iter().enumerate() {
                d[id as usize] = i as u32 + 1;
            }
            TypePos::Dense(d)
        } else {
            TypePos::Map(ids.iter().enumerate().map(|(i, &id)| (id, i as u32)).collect())
        };
        Types { ids, pos, raw, offs, cells }
    }

    pub fn from_map(m: FxHashMap<u32, TypeInfo>) -> Types {
        let mut v: Vec<(u32, TypeInfo)> = m.into_iter().collect();
        v.sort_unstable_by_key(|x| x.0);
        let mut raw = Vec::new();
        let mut offs = vec![0u32];
        for (_, t) in &v {
            t.pack(&mut raw);
            offs.push(raw.len() as u32);
        }
        let ids = v.iter().map(|x| x.0).collect();
        let cells = v.into_iter().map(|(_, t)| std::sync::OnceLock::from(Box::new(t))).collect();
        Types::build(ids, raw, offs, cells)
    }

    #[inline]
    fn index(&self, id: u32) -> Option<usize> {
        match &self.pos {
            TypePos::Dense(d) => match d.get(id as usize) {
                Some(&k) if k > 0 => Some(k as usize - 1),
                _ => None,
            },
            TypePos::Map(m) => m.get(&id).map(|&k| k as usize),
        }
    }

    fn at(&self, i: usize) -> &TypeInfo {
        self.cells[i].get_or_init(|| {
            let rec = &self.raw[self.offs[i] as usize..self.offs[i + 1] as usize];
            match TypeInfo::unpack(rec) {
                Some(t) if t.id == self.ids[i] => Box::new(t),
                _ => panic!("corrupt dataset cache entry for type {} (delete the eve-dogma cache directory)", self.ids[i]),
            }
        })
    }

    #[inline]
    pub fn get(&self, id: &u32) -> Option<&TypeInfo> {
        self.index(*id).map(|i| self.at(i))
    }

    pub fn contains_key(&self, id: &u32) -> bool {
        self.index(*id).is_some()
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// all types in ascending id order (decodes every record)
    pub fn iter(&self) -> impl Iterator<Item = (&u32, &TypeInfo)> {
        self.ids.iter().enumerate().map(move |(i, id)| (id, self.at(i)))
    }
}

impl std::ops::Index<&u32> for Types {
    type Output = TypeInfo;
    fn index(&self, id: &u32) -> &TypeInfo {
        self.get(id).expect("unknown type id")
    }
}

impl Serialize for Types {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let ids: Vec<u8> = self.ids.iter().flat_map(|x| x.to_le_bytes()).collect();
        let offs: Vec<u8> = self.offs.iter().flat_map(|x| x.to_le_bytes()).collect();
        (RawBytes(ids), RawBytes(offs), RawBytes(self.raw.clone())).serialize(s)
    }
}

impl<'de> Deserialize<'de> for Types {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let (ids, offs, raw) = <(RawBytes, RawBytes, RawBytes)>::deserialize(d)?;
        let u32s = |b: &[u8]| -> Result<Vec<u32>, D::Error> {
            if b.len() % 4 != 0 {
                return Err(D::Error::custom("packed u32 length"));
            }
            Ok(b.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
        };
        let (ids, offs, raw) = (u32s(&ids.0)?, u32s(&offs.0)?, raw.0);
        // structural checks; a record itself is checked when decoded
        if offs.len() != ids.len() + 1
            || offs.first() != Some(&0)
            || offs.last().map(|&x| x as usize) != Some(raw.len())
            || offs.windows(2).any(|w| w[0] > w[1])
            || ids.windows(2).any(|w| w[0] >= w[1])
        {
            return Err(D::Error::custom("bad type table"));
        }
        let cells = (0..ids.len()).map(|_| std::sync::OnceLock::new()).collect();
        Ok(Types::build(ids, raw, offs, cells))
    }
}

/// bytes (de)serialized as one block: bincode copies them without per-element visits or UTF-8 validation
#[derive(Default)]
struct RawBytes(Vec<u8>);

impl Serialize for RawBytes {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for RawBytes {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = RawBytes;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("bytes")
            }
            fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<RawBytes, E> {
                Ok(RawBytes(v.to_vec()))
            }
            fn visit_byte_buf<E: serde::de::Error>(self, v: Vec<u8>) -> Result<RawBytes, E> {
                Ok(RawBytes(v))
            }
        }
        d.deserialize_byte_buf(V)
    }
}

#[derive(Serialize, Deserialize)]
pub struct Dataset {
    pub build: u64,
    pub release_date: Option<String>,
    pub sha256: String,
    pub types: Types,
    pub groups: FxHashMap<u32, GroupInfo>,
    /// category id -> English name
    pub categories: FxHashMap<u32, String>,
    pub attrs: Dense<AttrInfo>,
    pub effects: Table<EffectInfo>,
    pub dbuffs: FxHashMap<u32, DbuffInfo>,
    pub mutaplasmids: FxHashMap<u32, MutaInfo>,
    /// Chinese type names as one "id\tname\n" blob sorted by id: one allocation to load from the binary cache
    /// instead of ~10k; indexed on first use (only the search/type commands read it)
    names_zh_raw: RawBytes,
    #[serde(skip)]
    names_zh_idx: std::sync::OnceLock<Vec<(u32, u32, u32)>>,
    /// per skill: (has an effect without modifiers, all modifiers of its effects except skillEffect), built on first use
    #[serde(skip)]
    skill_mods: std::sync::OnceLock<FxHashMap<u32, (bool, Box<[Modifier]>)>>,
    /// name -> id indexes, built on first use from the attribute / effect names (not stored in the binary cache)
    #[serde(skip)]
    attr_by_name: std::sync::OnceLock<NameIndex>,
    #[serde(skip)]
    effect_by_name: std::sync::OnceLock<NameIndex>,
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
    /// type ids of the Gunnery and Missile Launcher Operation skills (0 if absent)
    pub skill_gunnery: u32,
    pub skill_mls: u32,
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
        for (k, a) in raw.attributes {
            let id: u32 = k.parse().unwrap_or(0);
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
        for (k, e) in raw.effects {
            let id: u32 = k.parse().unwrap_or(0);
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
        let names_zh_raw = {
            let mut v: Vec<(u32, &String)> =
                raw.names.get("zh").map(|m| m.iter().map(|(k, v)| (k.parse().unwrap_or(0), v)).collect()).unwrap_or_default();
            v.sort_unstable_by_key(|x| x.0);
            v.dedup_by_key(|x| x.0);
            let mut s = String::new();
            for (id, n) in v {
                if n.contains(['\t', '\n']) {
                    continue;
                }
                s.push_str(&format!("{id}\t{n}\n"));
            }
            RawBytes(s.into_bytes())
        };
        Ok(Dataset {
            build: raw.sde.build,
            release_date: raw.sde.release_date,
            sha256,
            types: Types::from_map(types),
            groups,
            categories,
            attrs,
            effects: Table::from_dense(effects, |e: &EffectInfo| e.name.as_str()),
            dbuffs,
            mutaplasmids,
            names_zh_raw,
            names_zh_idx: std::sync::OnceLock::new(),
            skill_mods: std::sync::OnceLock::new(),
            attr_by_name: std::sync::OnceLock::new(),
            effect_by_name: std::sync::OnceLock::new(),
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
                skill_gunnery: d.type_by_name("Gunnery").unwrap_or(0),
                skill_mls: d.type_by_name("Missile Launcher Operation").unwrap_or(0),
            };
            d
        })
    }

    /// modifiers a skill's effects can apply (effect 132 skillEffect excluded) and whether any effect is hand-written
    /// (no modifiers); None for a type that is not a skill
    pub fn skill_mods(&self, skill: u32) -> Option<(bool, &[Modifier])> {
        let m = self.skill_mods.get_or_init(|| {
            let mut m = FxHashMap::with_capacity_and_hasher(self.skills.len(), Default::default());
            for &s in &self.skills {
                let Some(t) = self.types.get(&s) else { continue };
                let (mut special, mut mods) = (false, Vec::new());
                for (eid, _) in &t.effects {
                    if *eid == 132 {
                        continue;
                    }
                    let Some(e) = self.effects.get(eid) else { continue };
                    if e.mods.is_empty() {
                        special = true;
                    }
                    mods.extend_from_slice(&e.mods);
                }
                m.insert(s, (special, mods.into_boxed_slice()));
            }
            m
        });
        m.get(&skill).map(|(sp, v)| (*sp, &v[..]))
    }

    /// attribute id by name (0 if unknown; for a duplicated name the lowest id)
    pub fn attr_id(&self, name: &str) -> u32 {
        let names = |id: u32| self.attrs.get(&id).map(|a| a.name.as_str());
        self.attr_by_name.get_or_init(|| NameIndex::build(self.attrs.iter().map(|(id, a)| (id, a.name.as_str())), names)).find(name, names)
    }
    /// effect id by name (0 if unknown; for a duplicated name the lowest id)
    pub fn effect_id(&self, name: &str) -> u32 {
        let names = |id: u32| self.effects.name(id);
        self.effect_by_name.get_or_init(|| NameIndex::build(self.effects.names(), names)).find(name, names)
    }
    /// Chinese name of a type, if the dataset has one
    pub fn name_zh(&self, id: u32) -> Option<&str> {
        let idx = self.names_zh_idx.get_or_init(|| {
            let mut v = Vec::new();
            let mut pos = 0usize;
            for line in self.names_zh_str().split_inclusive('\n') {
                if let Some((k, n)) = line.trim_end_matches('\n').split_once('\t') {
                    let start = pos + k.len() + 1;
                    v.push((k.parse().unwrap_or(0), start as u32, (start + n.len()) as u32));
                }
                pos += line.len();
            }
            v
        });
        // the blob was validated as UTF-8 once when the index was built: per lookup only the entry's bytes are checked
        // (validating the whole blob per call made `search` quadratic, seconds per call)
        let k = idx.binary_search_by_key(&id, |x| x.0).ok()?;
        std::str::from_utf8(&self.names_zh_raw.0[idx[k].1 as usize..idx[k].2 as usize]).ok()
    }

    fn names_zh_str(&self) -> &str {
        std::str::from_utf8(&self.names_zh_raw.0).unwrap_or("")
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

