//! Data-driven dogma engine: builds the object graph for one request, registers modifiers, evaluates lazily.
use crate::data::{Dataset, Domain, Func, Modifier, TypeInfo};
use crate::request::{FitRequest, ModuleReq, Slot, State};
use rustc_hash::FxHashMap;
use std::cell::Cell;

/// Source categories exempt from stacking penalties: Ship, Charge, Skill, Implant, Subsystem, Structure.
const EXEMPT_CATEGORIES: [u32; 6] = [6, 8, 16, 20, 32, 65];
/// requiredSkill1..6
pub const REQ_SKILL_ATTRS: [u32; 6] = [182, 183, 184, 1285, 1289, 1290];
pub const ATTR_SKILL_LEVEL: u32 = 280;
const EFFECT_SKILL_EFFECT: u32 = 132;
const SKILL_ATTR_CAP: usize = 3;
/// em/explosive/kinetic/thermal DamageResonance (hull)
const HULL_RESONANCES: [u32; 4] = [113, 111, 109, 110];
/// On structures (category 65) pilot skills do not affect the structure, except these effects
/// (max locked targets + skillStructure* bonuses). Matches observed game/Pyfa behaviour.
const STRUCTURE_SKILL_EFFECT_NAMES: [&str; 5] = [
    "targetingMaxTargetBonusModAddMaxLockedTargetsLocationChar",
    "skillStructureMissileDamageBonus",
    "skillStructureElectronicSystemsCapNeedBonus",
    "skillStructureEngineeringSystemsCapNeedBonus",
    "skillStructureDoomsdayDurationBonus",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ship,
    Char,
    Skill,
    Module,
    Charge,
    Drone,
    Fighter,
    Implant,
    Booster,
    Mode,
    Beacon,
    Projected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loc {
    Ship,
    Char,
    Space,
    Nowhere,
}

#[derive(Debug, Clone, Copy)]
pub enum Src {
    /// value of attribute `attr` on item `item`
    Attr { item: usize, attr: u32 },
    Const(f64),
    /// AB/MWD: 1 + speedFactor/100 * speedBoostFactor / ship mass  (PostMul)
    Prop { module: usize, ship: usize, speed: u32, thrust: u32, mass: u32 },
    /// projected: value scaled by range factor and (lazily) by target resistance attribute
    Projected { item: usize, attr: u32, factor: f64, target: usize, resist: u32, mul: bool },
}

/// stacking penalty factor exp(-(i^2) / 7.1289) of the i-th strongest modifier (memoised; bit-identical)
#[inline]
fn stack_factor(i: usize) -> f64 {
    static T: std::sync::OnceLock<[f64; 16]> = std::sync::OnceLock::new();
    let f = |i: usize| (-((i * i) as f64) / 7.1289).exp();
    if i < 16 {
        T.get_or_init(|| std::array::from_fn(f))[i]
    } else {
        f(i)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AMod {
    pub op: i32,
    pub penalized: bool,
    pub src: Src,
    pub source_item: usize,
}

#[derive(Debug)]
pub struct Attr {
    pub base: f64,
    pub mods: smallvec::SmallVec<[AMod; 1]>,
    val: Cell<Option<f64>>,
    busy: Cell<bool>,
}

impl Attr {
    fn new(base: f64) -> Attr {
        Attr { base, mods: smallvec::SmallVec::new(), val: Cell::new(None), busy: Cell::new(false) }
    }
}

#[derive(Debug)]
pub struct Item<'a> {
    pub type_id: u32,
    pub group: u32,
    pub category: u32,
    pub kind: Kind,
    pub state: State,
    pub loc: Loc,
    pub owned: bool,
    pub parent: Option<usize>,
    pub charge: Option<usize>,
    pub slot: Option<Slot>,
    /// index into the request list this item came from (modules/drones/...)
    pub req_index: Option<usize>,
    pub quantity: u32,
    pub active_count: u32,
    /// the type's base attributes (sorted by id, borrowed from the dataset); only attributes that are
    /// overridden or modified get an entry in `attrs`, which takes precedence
    pub tattrs: &'a [(u32, f64)],
    pub attrs: FxHashMap<u32, Attr>,
    pub req_skills: std::borrow::Cow<'a, [u32]>,
    /// effect ids carried by this item (own + mutation base); borrowed from the dataset unless mutated
    pub effects: std::borrow::Cow<'a, [(u32, bool)]>,
    pub fighter_abilities: Option<Vec<u32>>,
    pub booster_side_effects: Vec<u32>,
    pub spool: Option<crate::request::Spool>,
    pub distance: Option<f64>,
}

pub struct Fit<'a> {
    pub ds: &'a Dataset,
    pub items: Vec<Item<'a>>,
    pub ship: usize,
    pub char: usize,
    pub warnings: Vec<String>,
    pub is_structure: bool,
    /// incoming remote reps / cap transfers / neuts from projected items, evaluated in stats
    pub proj_special: Vec<ProjSpecial>,
    /// target index for location/group/skill filtered modifiers (built once the item set is final)
    tindex: Option<TIndex>,
    /// see `ship_touched`; None = no pruning (attributes requested)
    ship_touched: Option<rustc_hash::FxHashSet<u32>>,
}

#[derive(Default)]
struct TIndex {
    ship_loc: Vec<usize>,
    ship_group: FxHashMap<u32, Vec<usize>>,
    ship_skill: FxHashMap<u32, Vec<usize>>,
    owned_skill: FxHashMap<u32, Vec<usize>>,
    char_loc: Vec<usize>,
    char_group: FxHashMap<u32, Vec<usize>>,
    char_skill: FxHashMap<u32, Vec<usize>>,
}

impl TIndex {
    fn build(items: &[Item<'_>]) -> TIndex {
        let mut t = TIndex::default();
        for (i, it) in items.iter().enumerate() {
            if it.loc == Loc::Ship {
                t.ship_loc.push(i);
                t.ship_group.entry(it.group).or_default().push(i);
                for s in it.req_skills.iter() {
                    let v = t.ship_skill.entry(*s).or_default();
                    if v.last() != Some(&i) {
                        v.push(i);
                    }
                }
            }
            if it.owned {
                for s in it.req_skills.iter() {
                    let v = t.owned_skill.entry(*s).or_default();
                    if v.last() != Some(&i) {
                        v.push(i);
                    }
                }
            }
            if it.loc == Loc::Char {
                t.char_loc.push(i);
                t.char_group.entry(it.group).or_default().push(i);
            }
            if (it.owned || it.loc == Loc::Char) && it.kind != Kind::Skill {
                for s in it.req_skills.iter() {
                    let v = t.char_skill.entry(*s).or_default();
                    if v.last() != Some(&i) {
                        v.push(i);
                    }
                }
            }
        }
        t
    }
}

/// A projected effect that does not modify attributes but feeds tank or capacitor stats (Pyfa: fit._armorRr, addDrain).
#[derive(Debug, Clone, Copy)]
pub enum ProjSpecial {
    /// layer 0 shield, 1 armor, 2 hull; amount attr * mult * factor every `duration`
    Rep { item: usize, layer: u8, amount: u32, mult: f64, factor: f64 },
    /// capacitor drain (sign +1) or fill (sign -1) per cycle of `duration` attr
    Drain { item: usize, amount: u32, duration: u32, factor: f64, resist: u32, sign: f64 },
    /// ECM jam strength vs the target's strongest sensor type (Pyfa addProjectedEcm / jamChance)
    Ecm { item: usize, fighter: bool, factor: f64, resist: u32 },
}

#[derive(Debug)]
pub struct EngineError {
    pub code: &'static str,
    pub message: String,
    pub path: String,
}

/// Pyfa default: standard attack on; other abilities (except MWD/evasive/MJD) on only if they come before the
/// standard attack in effect order.
fn default_fighter_abilities(ds: &Dataset, effects: &[(u32, bool)]) -> Vec<u32> {
    let mut ids: Vec<u32> = effects.iter().map(|(e, _)| *e).collect();
    ids.sort();
    let mut on = Vec::new();
    let mut std_seen = false;
    for e in ids {
        let Some(n) = ds.effects.get(&e).map(|x| x.name.as_str()) else { continue };
        if !n.starts_with("fighterAbility") {
            continue;
        }
        if n == "fighterAbilityAttackM" {
            on.push(e);
            std_seen = true;
        } else if !std_seen && !matches!(n, "fighterAbilityMicroWarpDrive" | "fighterAbilityEvasiveManeuvers" | "fighterAbilityMicroJumpDrive") {
            on.push(e);
        }
    }
    on
}

/// Skills required by any item of the request and the groups present (for skill pruning).
fn fit_skill_context(ds: &Dataset, req: &FitRequest) -> (rustc_hash::FxHashSet<u32>, rustc_hash::FxHashSet<u32>) {
    let mut need = rustc_hash::FxHashSet::with_capacity_and_hasher(128, Default::default());
    let mut groups = rustc_hash::FxHashSet::with_capacity_and_hasher(64, Default::default());
    let mut add = |tid: u32| {
        if let Some(t) = ds.types.get(&tid) {
            groups.insert(t.group);
            for a in REQ_SKILL_ATTRS {
                if let Some(v) = t.attr(a) {
                    if v != 0.0 {
                        need.insert(v as u32);
                    }
                }
            }
        }
    };
    add(req.ship.type_id);
    if let Some(m) = req.ship.mode_type_id {
        add(m);
    }
    for m in &req.modules {
        add(m.type_id);
        if let Some(c) = m.charge_type_id {
            add(c);
        }
        if let Some(mu) = &m.mutation {
            add(mu.base_type_id);
        }
    }
    for d in &req.drones {
        add(d.type_id);
        if let Some(mu) = &d.mutation {
            add(mu.base_type_id);
        }
    }
    for f in &req.fighters {
        add(f.type_id);
    }
    for i in &req.implants {
        add(*i);
    }
    for b in &req.boosters {
        add(b.type_id);
    }
    for c in &req.cargo {
        add(c.type_id);
    }
    (need, groups)
}

/// Ship attributes that something other than a skill may modify (any type in the request, incl. projected and
/// environment items, plus every warfare buff's ship attributes). Used to drop skill modifiers onto ship attributes
/// the ship does not have and nothing else touches: they cannot change any computed stat (only the
/// `include_attributes` listing, so the pruning is off when attributes are requested).
pub(crate) fn ship_touched(ds: &Dataset, req: &FitRequest) -> rustc_hash::FxHashSet<u32> {
    let mut out = rustc_hash::FxHashSet::with_capacity_and_hasher(128, Default::default());
    let add = |tid: u32, out: &mut rustc_hash::FxHashSet<u32>| {
        if let Some(t) = ds.types.get(&tid) {
            for (eid, _) in t.effects.iter() {
                if let Some(e) = ds.effects.get(eid) {
                    for m in &e.mods {
                        if m.func == Func::Item && !matches!(m.domain, Domain::Item) {
                            out.insert(m.modified);
                        }
                    }
                }
            }
        }
    };
    fn walk(req: &FitRequest, f: &mut dyn FnMut(u32)) {
        f(req.ship.type_id);
        if let Some(m) = req.ship.mode_type_id {
            f(m);
        }
        for m in &req.modules {
            f(m.type_id);
            if let Some(c) = m.charge_type_id {
                f(c);
            }
            if let Some(mu) = &m.mutation {
                f(mu.base_type_id);
            }
        }
        for d in &req.drones {
            f(d.type_id);
        }
        for x in &req.fighters {
            f(x.type_id);
        }
        for i in &req.implants {
            f(*i);
        }
        for b in &req.boosters {
            f(b.type_id);
        }
        for e in &req.environment.effect_type_ids {
            f(*e);
        }
        for p in &req.projected {
            if let Some(m) = &p.module {
                f(m.type_id);
                if let Some(c) = m.charge_type_id {
                    f(c);
                }
            }
            if let Some(d) = &p.drone {
                f(d.type_id);
            }
            if let Some(x) = &p.fighter {
                f(x.type_id);
            }
            if let Some(pf) = &p.fit {
                walk(pf, f);
            }
        }
        for bf in &req.fleet.booster_fits {
            walk(bf, f);
        }
    }
    let mut ids = Vec::new();
    walk(req, &mut |t| ids.push(t));
    for t in ids {
        add(t, &mut out);
    }
    for (_, b) in ds.dbuffs.iter() {
        out.extend(b.item.iter().copied());
    }
    out
}

/// Can any modifier of skill `s` reach an item of this fit? Conservative: unknown shapes count as relevant.
fn skill_relevant(ds: &Dataset, s: u32, need: &rustc_hash::FxHashSet<u32>, groups: &rustc_hash::FxHashSet<u32>, prune: Option<(&TypeInfo, &rustc_hash::FxHashSet<u32>)>) -> bool {
    if need.contains(&s) {
        return true;
    }
    // skills: precomputed modifier list; any other type id given as a skill: walk its effects
    let owned: Vec<Modifier>;
    let (special, mods): (bool, &[Modifier]) = match ds.skill_mods(s) {
        Some(x) => x,
        None => {
            let Some(t) = ds.types.get(&s) else { return false };
            let mut special = false;
            let mut v = Vec::new();
            for (eid, _) in &t.effects {
                if *eid == EFFECT_SKILL_EFFECT {
                    continue;
                }
                let Some(e) = ds.effects.get(eid) else { continue };
                special |= e.mods.is_empty();
                v.extend_from_slice(&e.mods);
            }
            owned = v;
            (special, &owned)
        }
    };
    if special {
        return true; // hand-written / special effect
    }
    for m in mods {
        let hit = match m.func {
            Func::Item => match prune {
                // self-modifiers only matter to the skill's own other effects; ship attributes that the ship
                // lacks and nothing else touches cannot affect a stat
                Some((ship, touched)) => match m.domain {
                    Domain::Item => false,
                    Domain::Ship => ship.attr(m.modified).is_some() || touched.contains(&m.modified),
                    _ => true,
                },
                None => true,
            },
            Func::Location | Func::EffectStopper => true,
            Func::LocationGroup => groups.contains(&m.extra) || ds.groups.get(&m.extra).map(|g| g.category == 16).unwrap_or(true),
            Func::LocationRequiredSkill | Func::OwnerRequiredSkill => need.contains(&if m.extra == 0 { s } else { m.extra }),
        };
        if hit {
            return true;
        }
    }
    false
}

fn state_ok(category: u8, state: State) -> bool {
    match category {
        0 | 4 => state >= State::Online,
        1 => state >= State::Active,
        5 => state >= State::Overheated,
        7 => true,
        _ => false, // 2 target, 3 area, 6 dungeon: not local
    }
}

impl<'a> Fit<'a> {
    fn new_item(&mut self, type_id: u32, kind: Kind, loc: Loc, path: std::fmt::Arguments) -> Result<usize, EngineError> {
        let ds = self.ds;
        let t = ds.types.get(&type_id).ok_or_else(|| EngineError {
            code: "UNKNOWN_TYPE",
            message: format!("unknown type_id {type_id}"),
            path: path.to_string(),
        })?;
        let mut item = Item {
            type_id,
            group: t.group,
            category: t.category,
            kind,
            state: State::Online,
            loc,
            owned: matches!(kind, Kind::Module | Kind::Charge | Kind::Drone | Kind::Fighter | Kind::Ship),
            parent: None,
            charge: None,
            slot: None,
            req_index: None,
            quantity: 1,
            active_count: 0,
            tattrs: &t.attrs,
            // modules / charges / drones typically get up to ~28 modified attributes: avoid the rehash steps
            attrs: match kind {
                Kind::Module | Kind::Charge | Kind::Drone => FxHashMap::with_capacity_and_hasher(28, Default::default()),
                // skill level plus the skill's self-modifiers
                Kind::Skill => FxHashMap::with_capacity_and_hasher(SKILL_ATTR_CAP, Default::default()),
                _ => FxHashMap::default(),
            },
            req_skills: std::borrow::Cow::Borrowed(&t.req_skills),
            effects: std::borrow::Cow::Borrowed(&t.effects),
            fighter_abilities: None,
            booster_side_effects: Vec::new(),
            spool: None,
            distance: None,
        };
        set_type_attrs(&mut item, t);
        self.items.push(item);
        Ok(self.items.len() - 1)
    }

    fn apply_mutation(&mut self, idx: usize, m: &crate::request::Mutation) {
        let ds = self.ds;
        if let Some(base) = ds.types.get(&m.base_type_id) {
            let own = self.items[idx].effects.clone();
            let item = &mut self.items[idx];
            // base attrs first, then mutated type's own attrs on top
            let own_attrs: Vec<(u32, f64)> = ds.types[&item.type_id].attrs.clone();
            for (a, v) in &base.attrs {
                item.attrs.insert(*a, Attr::new(*v));
            }
            for (a, v) in own_attrs {
                item.attrs.insert(a, Attr::new(v));
            }
            for (e, d) in &base.effects {
                if !own.iter().any(|(x, _)| x == e) {
                    item.effects.to_mut().push((*e, *d));
                }
            }
            if item.req_skills.is_empty() {
                item.req_skills = std::borrow::Cow::Borrowed(&base.req_skills);
            }
            if item.base_opt(4).unwrap_or(0.0) == 0.0 && base.mass != 0.0 {
                item.attrs.insert(4, Attr::new(base.mass));
            }
        }
        // rolled values (absolute), clamped to mutaplasmid range when known
        let muta = m.mutaplasmid_type_id.and_then(|id| ds.mutaplasmids.get(&id));
        let base_t = ds.types.get(&m.base_type_id);
        for (k, v) in &m.attributes {
            let Ok(aid) = k.parse::<u32>() else { continue };
            let mut val = *v;
            if let (Some(mu), Some(bt)) = (muta, base_t) {
                if let (Some((lo, hi)), Some(bv)) = (mu.attrs.get(k), bt.attr(aid)) {
                    let (a, b) = (bv * lo, bv * hi);
                    let (mn, mx) = if a < b { (a, b) } else { (b, a) };
                    if bv != 0.0 {
                        val = val.clamp(mn, mx);
                    }
                }
            }
            self.items[idx].attrs.insert(aid, Attr::new(val));
        }
    }

    fn add_module(&mut self, i: usize, m: &ModuleReq, path: std::fmt::Arguments) -> Result<usize, EngineError> {
        let idx = self.new_item(m.type_id, Kind::Module, Loc::Ship, path)?;
        let slot = m.slot.or_else(|| infer_slot(self.ds, &self.ds.types[&m.type_id]));
        let it = &mut self.items[idx];
        it.slot = slot;
        it.req_index = Some(i);
        it.spool = m.spool;
        it.state = m.state.unwrap_or(match slot {
            Some(Slot::Rig) | Some(Slot::Subsystem) => State::Online,
            _ => State::Online,
        });
        if matches!(slot, Some(Slot::Rig) | Some(Slot::Subsystem)) && it.state != State::Offline {
            it.state = State::Online;
        }
        if let Some(mu) = &m.mutation {
            self.apply_mutation(idx, mu);
        }
        if let Some(c) = m.charge_type_id {
            let cidx = self.new_item(c, Kind::Charge, Loc::Ship, format_args!("{path}/charge_type_id"))?;
            self.items[cidx].parent = Some(idx);
            self.items[cidx].req_index = Some(i);
            self.items[idx].charge = Some(cidx);
        }
        Ok(idx)
    }

    /// Build the object graph for a request. Does not evaluate anything.
    pub fn build(ds: &'a Dataset, req: &FitRequest) -> Result<Fit<'a>, EngineError> {
        let mut fit = Fit { ds, items: Vec::with_capacity(512), ship: 0, char: 0, warnings: Vec::new(), is_structure: false, proj_special: Vec::new(), tindex: None, ship_touched: None };
        let ship = fit.new_item(req.ship.type_id, Kind::Ship, Loc::Ship, format_args!("/ship/type_id"))?;
        fit.ship = ship;
        fit.is_structure = fit.items[ship].category == 65;
        let ch = fit.new_item(1373, Kind::Char, Loc::Char, format_args!("/character"))?;
        fit.char = ch;
        if let Some(sec) = req.character.security_status {
            let a = ds.attr_id("pilotSecurityStatus");
            if a != 0 {
                fit.items[ch].attrs.insert(a, Attr::new(sec));
            }
        }
        // skills
        let default_level = req.character.skills.default_level.unwrap_or(0);
        // every published skill exists (untrained = level 0): ship-bonus attrs like shipBonusGC2 are
        // scaled by a skill-level PreMul on the skill, so a missing skill would leave the raw per-level value.
        // Request levels override (last one wins per id); the result is sorted by skill id.
        let mut over: Vec<(u32, u8)> = Vec::new();
        for (k, v) in &req.character.skills.levels {
            if let Ok(id) = k.parse::<u32>() {
                over.push((id, *v));
            } else if let Some(id) = ds.type_by_name(k) {
                over.push((id, *v));
            }
        }
        over.reverse(); // stable sort + dedup keeps the first = the last inserted
        over.sort_by_key(|x| x.0);
        over.dedup_by_key(|x| x.0);
        let base = &ds.wk.published_skills;
        let mut lv: Vec<(u32, u8)> = Vec::with_capacity(base.len() + over.len());
        let (mut a, mut b) = (0, 0);
        while a < base.len() || b < over.len() {
            if b >= over.len() || (a < base.len() && base[a] < over[b].0) {
                lv.push((base[a], default_level));
                a += 1;
            } else {
                if a < base.len() && base[a] == over[b].0 {
                    a += 1;
                }
                lv.push(over[b]);
                b += 1;
            }
        }
        let (need, groups) = fit_skill_context(ds, req);
        let touched = if req.options.include_attributes.is_none() { Some(ship_touched(ds, req)) } else { None };
        let ship_t = ds.types.get(&req.ship.type_id);
        let prune = match (&touched, ship_t) {
            (Some(t), Some(st)) => Some((st, t)),
            _ => None,
        };
        for (s, l) in lv {
            if !ds.types.contains_key(&s) {
                continue;
            }
            // perf: a skill whose modifiers can reach nothing in this fit is not instantiated (same results)
            if !skill_relevant(ds, s, &need, &groups, prune) {
                continue;
            }
            let idx = fit.new_item(s, Kind::Skill, Loc::Char, format_args!("/character/skills"))?;
            fit.items[idx].attrs.insert(ATTR_SKILL_LEVEL, Attr::new(l.min(5) as f64));
            fit.items[idx].owned = false;
        }
        fit.ship_touched = touched;
        // Tactical destroyers must have a mode: default to the first (lowest type id) like Pyfa / the client.
        let mode_id = req.ship.mode_type_id.or_else(|| {
            let ship_name = ds.types.get(&req.ship.type_id)?.name.to_lowercase();
            // load-time list of mode types sorted by id: the first match is the lowest id
            let m = ds.wk.mode_types.iter().find(|(_, n)| n.starts_with(&ship_name)).map(|(id, _)| *id)?;
            fit.warnings.push(format!("no tactical mode given; defaulted to type {m}"));
            Some(m)
        });
        if let Some(mode) = mode_id {
            let idx = fit.new_item(mode, Kind::Mode, Loc::Nowhere, format_args!("/ship/mode_type_id"))?;
            fit.items[idx].owned = false;
        }
        for (i, m) in req.modules.iter().enumerate() {
            fit.add_module(i, m, format_args!("/modules/{i}"))?;
        }
        for (i, d) in req.drones.iter().enumerate() {
            let idx = fit.new_item(d.type_id, Kind::Drone, Loc::Space, format_args!("/drones/{i}"))?;
            if let Some(mu) = &d.mutation {
                fit.apply_mutation(idx, mu);
            }
            let it = &mut fit.items[idx];
            it.quantity = d.quantity.max(1);
            it.active_count = d.active.unwrap_or(0).min(it.quantity);
            it.state = if it.active_count > 0 { State::Active } else { State::Offline };
            it.req_index = Some(i);
        }
        for (i, f) in req.fighters.iter().enumerate() {
            let idx = fit.new_item(f.type_id, Kind::Fighter, Loc::Space, format_args!("/fighters/{i}"))?;
            let sq = ds.attr_id("fighterSquadronMaxSize");
            let maxsq = fit.items[idx].base_opt(sq).map(|a| a as u32).unwrap_or(1);
            let it = &mut fit.items[idx];
            it.quantity = f.quantity.unwrap_or(maxsq).clamp(1, maxsq.max(1));
            if f.quantity.unwrap_or(0) > maxsq {
                fit.warnings.push(format!("fighters/{i}: squadron size {} capped to {maxsq}", f.quantity.unwrap_or(0)));
            }
            it.active_count = if f.active { it.quantity } else { 0 };
            it.state = if f.active { State::Active } else { State::Offline };
            it.fighter_abilities = f.abilities.clone().or_else(|| Some(default_fighter_abilities(ds, &it.effects)));
            it.req_index = Some(i);
        }
        for (i, imp) in req.implants.iter().enumerate() {
            let idx = fit.new_item(*imp, Kind::Implant, Loc::Char, format_args!("/implants/{i}"))?;
            fit.items[idx].owned = false;
            fit.items[idx].req_index = Some(i);
        }
        for (i, b) in req.boosters.iter().enumerate() {
            let idx = fit.new_item(b.type_id, Kind::Booster, Loc::Char, format_args!("/boosters/{i}"))?;
            fit.items[idx].owned = false;
            fit.items[idx].booster_side_effects = b.side_effects.clone();
            fit.items[idx].req_index = Some(i);
        }
        for (i, e) in req.environment.effect_type_ids.iter().enumerate() {
            let idx = fit.new_item(*e, Kind::Beacon, Loc::Nowhere, format_args!("/environment/effect_type_ids/{i}"))?;
            fit.items[idx].owned = false;
        }
        for (i, p) in req.projected.iter().enumerate() {
            match p.kind.as_str() {
                "module" => {
                    if let Some(m) = &p.module {
                        for _ in 0..p.amount.max(1) {
                            let idx = fit.new_item(m.type_id, Kind::Projected, Loc::Nowhere, format_args!("/projected/{i}"))?;
                            let it = &mut fit.items[idx];
                            it.owned = false;
                            it.state = m.state.unwrap_or(State::Active);
                            it.distance = p.distance_m;
                            it.req_index = Some(i);
                            if let Some(c) = m.charge_type_id {
                                let cidx = fit.new_item(c, Kind::Charge, Loc::Nowhere, format_args!("/projected/{i}/module/charge_type_id"))?;
                                fit.items[cidx].parent = Some(idx);
                                fit.items[cidx].owned = false;
                                fit.items[idx].charge = Some(cidx);
                            }
                        }
                    }
                }
                "drone" => {
                    if let Some(d) = &p.drone {
                        for _ in 0..(p.amount.max(1) * d.quantity.max(1)) {
                            let idx = fit.new_item(d.type_id, Kind::Projected, Loc::Nowhere, format_args!("/projected/{i}"))?;
                            let it = &mut fit.items[idx];
                            it.owned = false;
                            it.state = State::Active;
                            it.distance = p.distance_m;
                        }
                    }
                }
                "fighter" => {
                    if let Some(f) = &p.fighter {
                        for _ in 0..p.amount.max(1) {
                            let idx = fit.new_item(f.type_id, Kind::Projected, Loc::Nowhere, format_args!("/projected/{i}"))?;
                            let sq = ds.attr_id("fighterSquadronMaxSize");
                            let maxsq = fit.items[idx].base_opt(sq).map(|a| a as u32).unwrap_or(1).max(1);
                            let it = &mut fit.items[idx];
                            it.owned = false;
                            it.state = if f.active { State::Active } else { State::Offline };
                            it.quantity = f.quantity.unwrap_or(maxsq).clamp(1, maxsq);
                            it.active_count = it.quantity;
                            it.distance = p.distance_m;
                            it.req_index = Some(i);
                            it.fighter_abilities = f.abilities.clone().or_else(|| Some(default_fighter_abilities(ds, &it.effects)));
                        }
                    }
                }
                "fit" => {
                    // whole projected fit: compute the source fit on its own (its skills, implants, fleet), then
                    // project each active module / drone as a frozen item carrying the source-modified values.
                    if let Some(src_req) = &p.fit {
                        let mut sreq = (**src_req).clone();
                        sreq.projected.clear();
                        let src = match Fit::build(ds, &sreq) {
                            Ok(f) => f,
                            Err(e) => {
                                fit.warnings.push(format!("projected[{i}] fit: {e:?}"));
                                continue;
                            }
                        };
                        let mut frozen: Vec<(u32, u32, FxHashMap<u32, f64>, Kind, u32, Option<Vec<u32>>)> = Vec::new();
                        for (si, it) in src.items.iter().enumerate() {
                            let copies = match it.kind {
                                Kind::Module if it.state >= State::Active => 1,
                                Kind::Drone => it.active_count,
                                Kind::Fighter if it.state >= State::Active => 1,
                                _ => 0,
                            };
                            if copies == 0 {
                                continue;
                            }
                            let vals: FxHashMap<u32, f64> = it.attr_ids().into_iter().map(|a| (a, src.get(si, a))).collect();
                            frozen.push((it.type_id, copies, vals, it.kind, it.quantity, it.fighter_abilities.clone()));
                        }
                        for (type_id, copies, vals, kind, qty, abil) in frozen {
                            for _ in 0..copies * p.amount.max(1) {
                                let idx = fit.new_item(type_id, Kind::Projected, Loc::Nowhere, format_args!("/projected/{i}"))?;
                                let it = &mut fit.items[idx];
                                it.owned = false;
                                it.state = State::Active;
                                it.distance = p.distance_m;
                                it.req_index = Some(i);
                                if kind == Kind::Fighter {
                                    it.quantity = qty;
                                    it.active_count = qty;
                                    it.fighter_abilities = abil.clone();
                                }
                                for (a, v) in &vals {
                                    it.attrs.entry(*a).or_insert_with(|| Attr::new(*v)).base = *v;
                                }
                            }
                        }
                    }
                }
                other => fit.warnings.push(format!("projected kind '{other}' not supported yet (index {i})")),
            }
        }
        // system security -> securityModifier (attr used by structure rigs etc.). Default nullsec, like Pyfa.
        {
            let sec = req.environment.system_security.as_deref().unwrap_or("nullsec").to_lowercase();
            let src = match sec.as_str() {
                "hisec" | "highsec" | "high" => "hiSecModifier",
                "lowsec" | "low" => "lowSecModifier",
                "nullsec" | "null" | "wspace" | "wormhole" | "w-space" => "nullSecModifier",
                other => {
                    fit.warnings.push(format!("unknown system_security '{other}', using nullsec"));
                    "nullSecModifier"
                }
            };
            let (src_id, dst_id) = (ds.attr_id(src), ds.attr_id("securityModifier"));
            for it in fit.items.iter_mut() {
                if let Some(v) = it.base_opt(src_id) {
                    it.attrs.insert(dst_id, Attr::new(v));
                }
            }
        }
        // attribute overrides (by type id, apply to all items of that type)
        for o in &req.overrides {
            for it in fit.items.iter_mut().filter(|it| it.type_id == o.type_id) {
                it.attrs.insert(o.attribute_id, Attr::new(o.value));
            }
        }
        fit.register_all(req);
        fit.apply_rah(req);
        Ok(fit)
    }

    // ---------------------------------------------------------------- registration
    fn push_mod(&mut self, target: usize, attr: u32, op: i32, src: Src, source_item: usize, source_cat: u32) {
        let ds = self.ds;
        let info = ds.attrs.get(&attr);
        let stackable = info.map(|a| a.stackable).unwrap_or(true);
        let penalized = !stackable && !EXEMPT_CATEGORIES.contains(&source_cat);
        let it = &mut self.items[target];
        let tattrs = it.tattrs;
        let a = it.attrs.entry(attr).or_insert_with(|| {
            let base = match tattrs.binary_search_by_key(&attr, |x| x.0) {
                Ok(k) => tattrs[k].1,
                Err(_) => info.map(|a| a.default).unwrap_or(0.0),
            };
            Attr::new(base)
        });
        a.mods.push(AMod { op, penalized, src, source_item });
    }

    fn targets(&self, src: usize, func: Func, domain: Domain, extra: u32) -> Vec<usize> {
        let mut out = Vec::new();
        self.targets_into(src, func, domain, extra, &mut out);
        out
    }

    /// `targets` into a caller-owned buffer (cleared first): no allocation per modifier on the registration path
    fn targets_into(&self, src: usize, func: Func, domain: Domain, extra: u32, out: &mut Vec<usize>) {
        out.clear();
        if let Some(t) = &self.tindex {
            let mut get = |m: &FxHashMap<u32, Vec<usize>>| {
                if let Some(v) = m.get(&extra) {
                    out.extend_from_slice(v)
                }
            };
            match (domain, func) {
                (Domain::Ship, Func::Location) => return out.extend_from_slice(&t.ship_loc),
                (Domain::Ship, Func::LocationGroup) => return get(&t.ship_group),
                (Domain::Ship, Func::LocationRequiredSkill) => return get(&t.ship_skill),
                (Domain::Ship, Func::OwnerRequiredSkill) => return get(&t.owned_skill),
                (Domain::Structure, _) if !self.is_structure => return,
                (Domain::Structure, Func::Location) => return out.extend_from_slice(&t.ship_loc),
                (Domain::Structure, Func::LocationGroup) => return get(&t.ship_group),
                (Domain::Structure, Func::LocationRequiredSkill) => return get(&t.ship_skill),
                (Domain::Structure, Func::OwnerRequiredSkill) => return get(&t.owned_skill),
                (Domain::Char, Func::Location) => return out.extend_from_slice(&t.char_loc),
                (Domain::Char, Func::LocationGroup) => return get(&t.char_group),
                (Domain::Char, Func::LocationRequiredSkill | Func::OwnerRequiredSkill) => return get(&t.char_skill),
                _ => {}
            }
        }
        let items = &self.items;
        let s = &items[src];
        match domain {
            Domain::Item => {
                if func == Func::Item {
                    out.push(src)
                }
            }
            Domain::Other => {
                if let Some(c) = s.charge {
                    out.push(c)
                } else if let Some(p) = s.parent {
                    out.push(p)
                }
            }
            Domain::Ship | Domain::Structure => {
                if domain == Domain::Structure && !self.is_structure {
                    return;
                }
                match func {
                    Func::Item => out.push(self.ship),
                    Func::Location | Func::LocationGroup | Func::LocationRequiredSkill => {
                        for (i, it) in items.iter().enumerate() {
                            if it.loc != Loc::Ship {
                                continue;
                            }
                            let ok = match func {
                                Func::Location => true,
                                Func::LocationGroup => it.group == extra,
                                _ => it.req_skills.contains(&extra),
                            };
                            if ok {
                                out.push(i)
                            }
                        }
                    }
                    Func::OwnerRequiredSkill => {
                        for (i, it) in items.iter().enumerate() {
                            if it.owned && it.req_skills.contains(&extra) {
                                out.push(i)
                            }
                        }
                    }
                    Func::EffectStopper => {}
                }
            }
            Domain::Char => match func {
                Func::Item => out.push(self.char),
                Func::Location | Func::LocationGroup => {
                    for (i, it) in items.iter().enumerate() {
                        if it.loc == Loc::Char && (func == Func::Location || it.group == extra) {
                            out.push(i)
                        }
                    }
                }
                Func::LocationRequiredSkill | Func::OwnerRequiredSkill => {
                    for (i, it) in items.iter().enumerate() {
                        if (it.owned || it.loc == Loc::Char) && it.kind != Kind::Skill && it.req_skills.contains(&extra) {
                            out.push(i)
                        }
                    }
                }
                Func::EffectStopper => {}
            },
            _ => {}
        }
    }

    fn effective_state(&self, i: usize) -> State {
        let it = &self.items[i];
        match it.kind {
            Kind::Charge => it.parent.map(|p| self.items[p].state).unwrap_or(State::Online),
            Kind::Ship | Kind::Char | Kind::Skill | Kind::Implant | Kind::Booster | Kind::Mode | Kind::Beacon => {
                State::Online
            }
            Kind::Drone | Kind::Fighter => {
                if it.active_count > 0 {
                    State::Active
                } else {
                    State::Offline
                }
            }
            _ => it.state,
        }
    }

    fn register_all(&mut self, req: &FitRequest) {
        self.tindex = Some(TIndex::build(&self.items));
        // pre-size the attribute maps of the items that collect many modifiers (avoids repeated rehashing)
        let (ship, ch) = (self.ship, self.char);
        self.items[ship].attrs.reserve(448);
        self.items[ch].attrs.reserve(48);
        let ds = self.ds;
        let n = self.items.len();
        let e_ab = ds.effect_id("moduleBonusAfterburner");
        let e_mwd = ds.effect_id("moduleBonusMicrowarpdrive");
        let e_slot = ds.effect_id("slotModifier");
        let e_hp = ds.effect_id("hardPointModifierEffect");
        let e_mjd = ds.effect_id("microJumpDrive");
        let e_bastion = ds.effect_id("moduleBonusBastionModule");
        let is_structure = self.items[self.ship].category == 65;
        let structure_ok: Vec<u32> = STRUCTURE_SKILL_EFFECT_NAMES.iter().map(|n| ds.effect_id(n)).collect();
        let mut tbuf: Vec<usize> = Vec::with_capacity(64);
        for i in 0..n {
            let kind = self.items[i].kind;
            if kind == Kind::Projected {
                self.register_projected(i);
                continue;
            }
            if is_structure && matches!(kind, Kind::Drone | Kind::Implant | Kind::Booster) {
                // structures ignore pilot implants/boosters and cannot use drones
                continue;
            }
            let state = self.effective_state(i);
            let src_cat = self.items[i].category;
            let effects = self.items[i].effects.clone();
            for &(eid, is_default) in effects.iter() {
                if eid == EFFECT_SKILL_EFFECT {
                    continue;
                }
                let Some(e) = ds.effects.get(&eid) else { continue };
                if is_structure
                    && kind == Kind::Skill
                    && !structure_ok.contains(&eid)
                    && !e.mods.iter().all(|m| m.domain == Domain::Item)
                {
                    continue;
                }
                // booster side effects only when selected
                if e.fitting_usage_chance_attr.is_some() && !self.items[i].booster_side_effects.contains(&eid) {
                    continue;
                }
                if kind == Kind::Fighter && e.category != 0 {
                    let used = match &self.items[i].fighter_abilities {
                        Some(a) => a.contains(&eid),
                        None => is_default,
                    };
                    if !used {
                        continue;
                    }
                }
                // Pyfa 'active' handlers for SDE effects without modifiers (some are target-category in the SDE)
                if e.mods.is_empty() && kind == Kind::Module && state >= State::Active && self.local_special(i, e.name.as_str(), src_cat) {
                    continue;
                }
                // engine-side unless the dataset carries the effect as stacking-exempt modifiers (revision 4+)
                if kind == Kind::Beacon && e.name == "OffensiveDefensiveReduction" && (e.mods.is_empty() || !e.stacking_exempt) {
                    self.incursion_effect(i);
                    continue;
                }
                if !state_ok(e.category, state) {
                    continue;
                }
                // ---- special effects (no modifierInfo in the SDE)
                if kind == Kind::Fighter && e.mods.is_empty() {
                    // fighter self abilities (Pyfa hand-written handlers, eos LGPL)
                    let mut fm: Vec<(&str, &str, i32)> = Vec::new();
                    match e.name.as_str() {
                        "fighterAbilityMicroWarpDrive" => {
                            fm.push(("maxVelocity", "fighterAbilityMicroWarpDriveSpeedBonus", 6));
                            fm.push(("signatureRadius", "fighterAbilityMicroWarpDriveSignatureRadiusBonus", 6));
                        }
                        "fighterAbilityAfterburner" => fm.push(("maxVelocity", "fighterAbilityAfterburnerSpeedBonus", 6)),
                        "fighterAbilityEvasiveManeuvers" => {
                            fm.push(("maxVelocity", "fighterAbilityEvasiveManeuversSpeedBonus", 6));
                            fm.push(("signatureRadius", "fighterAbilityEvasiveManeuversSignatureRadiusBonus", 6));
                            fm.push(("shieldEmDamageResonance", "fighterAbilityEvasiveManeuversEmResonance", 4));
                            fm.push(("shieldThermalDamageResonance", "fighterAbilityEvasiveManeuversThermResonance", 4));
                            fm.push(("shieldKineticDamageResonance", "fighterAbilityEvasiveManeuversKinResonance", 4));
                            fm.push(("shieldExplosiveDamageResonance", "fighterAbilityEvasiveManeuversExpResonance", 4));
                        }
                        _ => {}
                    }
                    if !fm.is_empty() {
                        for (t, a, op) in fm {
                            self.push_mod(i, ds.attr_id(t), op, Src::Attr { item: i, attr: ds.attr_id(a) }, i, src_cat);
                        }
                        continue;
                    }
                }
                if eid == e_ab || eid == e_mwd {
                    let ship = self.ship;
                    self.push_mod(ship, 4, 2, Src::Attr { item: i, attr: ds.attr_id("massAddition") }, i, src_cat);
                    let src = Src::Prop {
                        module: i,
                        ship,
                        speed: ds.attr_id("speedFactor"),
                        thrust: ds.attr_id("speedBoostFactor"),
                        mass: 4,
                    };
                    self.push_mod(ship, ds.attr_id("maxVelocity"), 4, src, i, src_cat);
                    if eid == e_mwd {
                        let a = ds.attr_id("signatureRadiusBonus");
                        self.push_mod(ship, ds.attr_id("signatureRadius"), 6, Src::Attr { item: i, attr: a }, i, src_cat);
                    }
                    continue;
                }
                if eid == e_mjd {
                    let a = ds.attr_id("signatureRadiusBonusPercent");
                    let ship = self.ship;
                    // MJD sig bloom is not stacking-penalised (unlike the MWD's)
                    self.push_mod(ship, ds.attr_id("signatureRadius"), 6, Src::Attr { item: i, attr: a }, i, 6);
                    continue;
                }
                if eid == e_slot {
                    let ship = self.ship;
                    for (t, s) in [("hiSlots", "hiSlotModifier"), ("medSlots", "medSlotModifier"), ("lowSlots", "lowSlotModifier")] {
                        self.push_mod(ship, ds.attr_id(t), 2, Src::Attr { item: i, attr: ds.attr_id(s) }, i, src_cat);
                    }
                    continue;
                }
                if eid == e_hp {
                    let ship = self.ship;
                    for (t, s) in [
                        ("turretSlotsLeft", "turretHardPointModifier"),
                        ("launcherSlotsLeft", "launcherHardPointModifier"),
                    ] {
                        self.push_mod(ship, ds.attr_id(t), 2, Src::Attr { item: i, attr: ds.attr_id(s) }, i, src_cat);
                    }
                    continue;
                }
                for m in &e.mods {
                    if m.func == Func::EffectStopper || m.op == 9 {
                        continue;
                    }
                    if matches!(m.domain, Domain::TargetId | Domain::Target) {
                        continue;
                    }
                    // a module without charge cannot reach otherID
                    // EXCT convention: skill filter 0 = the type owning the effect (skill self-bonuses)
                    let extra = if m.extra == 0 && matches!(m.func, Func::LocationRequiredSkill | Func::OwnerRequiredSkill) {
                        self.items[i].type_id
                    } else {
                        m.extra
                    };
                    // Bastion hull resists are not stacking penalised in game (observed by Pyfa); SDE marks the attrs non-stackable
                    // category 6 is never stacking-penalised: used for effects flagged stacking_exempt in the dataset
                    let cat = if e.stacking_exempt || eid == e_bastion && HULL_RESONANCES.contains(&m.modified) { 6 } else { src_cat };
                    if m.domain == Domain::Item && m.func == Func::Item {
                        // self-modifier (most skill effects): no target list needed
                        self.push_mod(i, m.modified, m.op, Src::Attr { item: i, attr: m.modifying }, i, cat);
                        continue;
                    }
                    if kind == Kind::Skill && m.func == Func::Item && m.domain == Domain::Ship {
                        let ship = self.ship;
                        if let Some(t) = &self.ship_touched {
                            if self.items[ship].tbase(m.modified).is_none() && !t.contains(&m.modified) {
                                continue; // see `ship_touched`
                            }
                        }
                    }
                    self.targets_into(i, m.func, m.domain, extra, &mut tbuf);
                    for &t in &tbuf {
                        self.push_mod(t, m.modified, m.op, Src::Attr { item: i, attr: m.modifying }, i, cat);
                    }
                }
            }
        }
        self.register_buffs(req);
    }

    fn register_projected(&mut self, i: usize) {
        const DAMAGE_EFFECTS: &[&str] = &["projectileFired", "targetAttack", "useMissiles", "barrage", "targetDisintegratorAttack",
            "missileLaunchingForEntity", "fighterAbilityAttackM", "fighterAbilityMissiles", "superWeaponAmarr", "superWeaponCaldari",
            "superWeaponGallente", "superWeaponMinmatar", "mining", "miningLaser", "miningClouds", "dotMissileLaunching", "ChainLightning", "salvageDroneEffect"];
        let ds = self.ds;
        let src_cat = self.items[i].category;
        let state = self.items[i].state;
        let effects = self.items[i].effects.clone();
        let ship = self.ship;
        let abilities = self.items[i].fighter_abilities.clone();
        let qty = self.items[i].quantity.max(1) as f64;
        for &(eid, _) in effects.iter() {
            let Some(e) = ds.effects.get(&eid) else { continue };
            if e.category != 2 && e.category != 3 && e.name != "ECMBurstJammer" && !e.name.starts_with("doomsdayAOE") {
                continue;
            }
            if let Some(ab) = &abilities {
                if e.name.starts_with("fighterAbility") && !ab.contains(&eid) {
                    continue;
                }
            }
            if state < State::Active {
                continue;
            }
            let factor = {
                let it = &self.items[i];
                let opt = e.range_attr.and_then(|a| it.base_opt(a)).unwrap_or(0.0);
                let fo = e.falloff_attr.and_then(|a| it.base_opt(a)).unwrap_or(0.0);
                crate::stats::range_factor(opt, fo, it.distance, true)
            };
            let resist = e.resistance_attr.unwrap_or_else(|| {
                let it = &self.items[i];
                let look = |n: &str| it.base_opt(ds.attr_id(n)).map(|a| a as u32).unwrap_or(0);
                if e.name.starts_with("fighterAbility") {
                    let r = look(&format!("{}ResistanceID", e.name));
                    if r != 0 { r } else { look(&format!("{}RemoteResistanceID", e.name)) }
                } else {
                    look("remoteResistanceID")
                }
            });
            let target_offense_ok = self.items[ship].base_opt(ds.attr_id("disallowOffensiveModifiers")).map(|a| a == 0.0).unwrap_or(true);
            let p_cat = if e.stacking_exempt { 6 } else { src_cat };
            let push = |fit: &mut Fit, target_attr: u32, src_attr: u32, op: i32| {
                let mul = op == 4 || op == 0;
                fit.push_mod(
                    ship,
                    target_attr,
                    op,
                    Src::Projected { item: i, attr: src_attr, factor, target: ship, resist, mul },
                    i,
                    p_cat,
                );
            };
            // burst projectors and the Standup weapon disruptor stay engine-side even if a dataset revision gives
            // them modifiers (eve-sde-pipeline proposed patch 0101): the generic path has no AoE full-strength rule
            let engine_side = e.name.starts_with("doomsdayAOE") || e.name == "structureModuleEffectWeaponDisruption";
            if !e.mods.is_empty() && !engine_side {
                for m in &e.mods {
                    if matches!(m.domain, Domain::TargetId | Domain::Target | Domain::Ship) && m.func == Func::Item {
                        push(self, m.modified, m.modifying, m.op);
                    }
                }
                continue;
            }
            let name = e.name.as_str();
            let pbase = |n: &str| self.items[i].base_opt(ds.attr_id(n)).unwrap_or(0.0);
            if name == "fighterAbilityStasisWebifier" {
                if target_offense_ok {
                    let f = crate::stats::range_factor(pbase("fighterAbilityStasisWebifierOptimalRange"), pbase("fighterAbilityStasisWebifierFalloffRange"), self.items[i].distance, true) * qty;
                    self.push_mod(ship, ds.attr_id("maxVelocity"), 6,
                        Src::Projected { item: i, attr: ds.attr_id("fighterAbilityStasisWebifierSpeedPenalty"), factor: f, target: ship, resist, mul: false }, i, src_cat);
                }
                continue;
            }
            if name == "fighterAbilityWarpDisruption" {
                if target_offense_ok && pbase("fighterAbilityWarpDisruptionRange") >= self.items[i].distance.unwrap_or(0.0) {
                    self.push_mod(ship, ds.attr_id("warpScrambleStatus"), 2,
                        Src::Projected { item: i, attr: ds.attr_id("fighterAbilityWarpDisruptionPointStrength"), factor: qty, target: ship, resist, mul: false }, i, src_cat);
                }
                continue;
            }
            // burst projectors (Pyfa Effect6476-6482/6513): full strength on every ship in the AoE (no range factor)
            let full = |fit: &mut Fit, t: usize, tgt: u32, sa: u32| {
                fit.push_mod(t, tgt, 6, Src::Projected { item: i, attr: sa, factor: 1.0, target: ship, resist, mul: false }, i, src_cat);
            };
            match name {
                "doomsdayAOEWeb" | "doomsdayAOEPaint" | "doomsdayAOEDamp" => {
                    if target_offense_ok {
                        let pairs: &[(&str, &str)] = match name {
                            "doomsdayAOEWeb" => &[("maxVelocity", "speedFactor")],
                            "doomsdayAOEPaint" => &[("signatureRadius", "signatureRadiusBonus")],
                            _ => &[("maxTargetRange", "maxTargetRangeBonus"), ("scanResolution", "scanResolutionBonus")],
                        };
                        for (t, sa) in pairs {
                            full(self, ship, ds.attr_id(t), ds.attr_id(sa));
                        }
                    }
                    continue;
                }
                "doomsdayAOENeut" => {
                    self.proj_special.push(ProjSpecial::Drain { item: i, amount: ds.attr_id("energyNeutralizerAmount"), duration: ds.attr_id("duration"), factor: 1.0, resist, sign: 1.0 });
                    continue;
                }
                "doomsdayAOEECM" => {
                    if target_offense_ok {
                        self.proj_special.push(ProjSpecial::Ecm { item: i, fighter: false, factor: 1.0, resist });
                    }
                    continue;
                }
                "doomsdayAOEBubble" | "doomsdayAOEGuide" => continue,
                _ => {}
            }
            let weapon_disruption = name == "doomsdayAOETrack" || name == "structureModuleEffectWeaponDisruption";
            if name.starts_with("remoteWebifier") || name == "structureModuleEffectStasisWebifier" {
                push(self, ds.attr_id("maxVelocity"), ds.attr_id("speedFactor"), 6);
            } else if name.starts_with("remoteTargetPaint") || name == "structureModuleEffectTargetPainter" {
                push(self, ds.attr_id("signatureRadius"), ds.attr_id("signatureRadiusBonus"), 6);
            } else if name.starts_with("remoteSensorDamp") || name == "structureModuleEffectRemoteSensorDampener" {
                push(self, ds.attr_id("maxTargetRange"), ds.attr_id("maxTargetRangeBonus"), 6);
                push(self, ds.attr_id("scanResolution"), ds.attr_id("scanResolutionBonus"), 6);
            } else if weapon_disruption {
                // AoE weapon disruption burst (full strength) / Standup Weapon Disruptor (range factor): turrets and missiles
                if target_offense_ok {
                    let tf = if name == "doomsdayAOETrack" {
                        1.0
                    } else {
                        let it = &self.items[i];
                        crate::stats::range_factor(it.base_opt(ds.attr_id("maxRange")).unwrap_or(0.0), it.base_opt(ds.attr_id("falloffEffectiveness")).unwrap_or(0.0), it.distance, true)
                    };
                    let (gun, mls) = (ds.wk.skill_gunnery, ds.wk.skill_mls);
                    let n = self.items.len();
                    for t in 0..n {
                        let it = &self.items[t];
                        if it.loc != Loc::Ship || !it.owned {
                            continue;
                        }
                        let pairs: &[(&str, &str)] = if it.kind == Kind::Module && it.req_skills.contains(&gun) {
                            &[("trackingSpeedBonus", "trackingSpeed"), ("maxRangeBonus", "maxRange"), ("falloffBonus", "falloff")]
                        } else if it.kind == Kind::Charge && it.req_skills.contains(&mls) {
                            &[("aoeCloudSizeBonus", "aoeCloudSize"), ("aoeVelocityBonus", "aoeVelocity"), ("missileVelocityBonus", "maxVelocity"), ("explosionDelayBonus", "explosionDelay")]
                        } else {
                            continue;
                        };
                        for (sa, ta) in pairs {
                            self.push_mod(t, ds.attr_id(ta), 6, Src::Projected { item: i, attr: ds.attr_id(sa), factor: tf, target: ship, resist, mul: false }, i, src_cat);
                        }
                    }
                }
            } else if name == "shipModuleTrackingDisruptor" || name == "shipModuleGuidanceDisruptor" || name == "shipModuleRemoteTrackingComputer" || name == "npcEntityWeaponDisruptor" {
                // Pyfa Effect6424 / Effect6423 / shipModuleRemoteTrackingComputer: boost the target's gunnery modules
                // (TD, remote tracking computer) / missile charges (GD)
                let allowed = if name == "shipModuleRemoteTrackingComputer" {
                    self.items[ship].base_opt(ds.attr_id("disallowAssistance")).map(|a| a == 0.0).unwrap_or(true)
                } else {
                    target_offense_ok
                };
                if allowed {
                    let (charges, pairs): (bool, &[(&str, &str)]) = if name != "shipModuleGuidanceDisruptor" {
                        (false, &[("trackingSpeedBonus", "trackingSpeed"), ("maxRangeBonus", "maxRange"), ("falloffBonus", "falloff")])
                    } else {
                        (true, &[("aoeCloudSizeBonus", "aoeCloudSize"), ("aoeVelocityBonus", "aoeVelocity"), ("missileVelocityBonus", "maxVelocity"), ("explosionDelayBonus", "explosionDelay")])
                    };
                    let sk = if charges { ds.wk.skill_mls } else { ds.wk.skill_gunnery };
                    let tf = if name == "npcEntityWeaponDisruptor" {
                        // TD drones (Pyfa Effect6694): full strength inside maxRange, nothing beyond
                        let it = &self.items[i];
                        if it.base_opt(ds.attr_id("maxRange")).unwrap_or(0.0) < it.distance.unwrap_or(0.0) { 0.0 } else { 1.0 }
                    } else {
                        let it = &self.items[i];
                        crate::stats::range_factor(it.base_opt(ds.attr_id("maxRange")).unwrap_or(0.0), it.base_opt(ds.attr_id("falloffEffectiveness")).unwrap_or(0.0), it.distance, true)
                    };
                    let targets: Vec<usize> = (0..self.items.len())
                        .filter(|&t| {
                            let it = &self.items[t];
                            it.loc == Loc::Ship && it.owned && if charges { it.kind == Kind::Charge } else { it.kind == Kind::Module } && it.req_skills.contains(&sk)
                        })
                        .collect();
                    for t in targets {
                        for (src_a, tgt_a) in pairs {
                            self.push_mod(t, ds.attr_id(tgt_a), 6, Src::Projected { item: i, attr: ds.attr_id(src_a), factor: tf, target: ship, resist, mul: false }, i, src_cat);
                        }
                    }
                }
            } else if name.starts_with("remoteSensorBoost") {
                push(self, ds.attr_id("maxTargetRange"), ds.attr_id("maxTargetRangeBonus"), 6);
                push(self, ds.attr_id("scanResolution"), ds.attr_id("scanResolutionBonus"), 6);
                for t in ["Gravimetric", "Ladar", "Magnetometric", "Radar"] {
                    push(self, ds.attr_id(&format!("scan{t}Strength")), ds.attr_id(&format!("scan{t}StrengthPercent")), 6);
                }
            } else if let Some(ps) = self.proj_special_for(i, name, resist) {
                if !ps.is_empty() {
                    self.proj_special.extend(ps);
                }
            } else if DAMAGE_EFFECTS.contains(&name) {
                // weapon damage onto the target: not part of the target's own stats
            } else {
                self.warnings.push(format!("projected effect '{name}' not modelled yet"));
            }
        }
    }

    /// Local module effects that have no modifierInfo in the SDE but a hand-written Pyfa handler (eos/effects.py,
    /// LGPL; re-expressed here). Returns true when the effect was handled. Category 6 as the source category marks
    /// a boost Pyfa applies without stacking penalty.
    fn local_special(&mut self, i: usize, name: &str, src_cat: u32) -> bool {
        let ds = self.ds;
        let ship = self.ship;
        let a = |n: &str| ds.attr_id(n);
        match name {
            "superWeaponAmarr" | "superWeaponCaldari" | "superWeaponGallente" | "superWeaponMinmatar" | "doomsdaySlash"
            | "doomsdayBeamDOT" | "doomsdayConeDOT" | "doomsdayHOG" | "debuffLance" => {
                self.push_mod(ship, a("maxVelocity"), 6, Src::Attr { item: i, attr: a("speedFactor") }, i, src_cat);
                self.push_mod(ship, a("warpScrambleStatus"), 2, Src::Attr { item: i, attr: a("siegeModeWarpStatus") }, i, src_cat);
            }
            "emergencyHullEnergizer" => {
                for t in ["Em", "Thermal", "Kinetic", "Explosive"] {
                    let tgt = a(&format!("{}DamageResonance", t.to_lowercase()));
                    self.push_mod(ship, tgt, 4, Src::Attr { item: i, attr: a(&format!("hull{t}DamageResonance")) }, i, src_cat);
                }
            }
            "entosisLink" => {
                self.push_mod(ship, a("disallowAssistance"), 7, Src::Attr { item: i, attr: a("disallowAssistance") }, i, 6);
                for t in ["Gravimetric", "Magnetometric", "Radar", "Ladar"] {
                    self.push_mod(ship, a(&format!("scan{t}Strength")), 6, Src::Attr { item: i, attr: a(&format!("scan{t}StrengthPercent")) }, i, src_cat);
                }
            }
            "moduleBonusBreacherPodDamageControl" => {
                self.push_mod(ship, a("breacherPodDamageResistance"), 6, Src::Attr { item: i, attr: a("breacherPodActivatedDamageReceivedPercentage") }, i, 6);
            }
            "microJumpPortalDrive" | "microJumpPortalDriveCapital" => {
                self.push_mod(ship, a("signatureRadius"), 6, Src::Attr { item: i, attr: a("signatureRadiusBonusPercent") }, i, src_cat);
            }
            "warpDisruptSphere" => {
                self.push_mod(ship, a("disallowAssistance"), 7, Src::Const(1.0), i, 6);
                if self.items[i].charge.is_none() {
                    self.push_mod(ship, 4, 6, Src::Attr { item: i, attr: a("massBonusPercentage") }, i, 6);
                    self.push_mod(ship, a("signatureRadius"), 6, Src::Attr { item: i, attr: a("signatureRadiusBonus") }, i, 6);
                    let props: Vec<usize> = (0..self.items.len())
                        .filter(|&t| {
                            let it = &self.items[t];
                            it.kind == Kind::Module && it.loc == Loc::Ship && ds.groups.get(&it.group).map(|g| g.name == "Propulsion Module").unwrap_or(false)
                        })
                        .collect();
                    for t in props {
                        self.push_mod(t, a("speedBoostFactor"), 6, Src::Attr { item: i, attr: a("speedBoostFactorBonus") }, i, 6);
                        self.push_mod(t, a("speedFactor"), 6, Src::Attr { item: i, attr: a("speedFactorBonus") }, i, 6);
                    }
                }
            }
            _ => return false,
        }
        true
    }

    /// Sansha / Drifter incursion system effects (Pyfa Effect4728 OffensiveDefensiveReduction, LGPL; re-expressed):
    /// unpenalised PostPercent of missile-charge and smartbomb damage, turret and drone damageMultiplier by
    /// systemEffectDamageReduction, and of the ship's armor/shield resonances by the beacon's resistance bonuses.
    fn incursion_effect(&mut self, b: usize) {
        let ds = self.ds;
        let a = |n: &str| ds.attr_id(n);
        let ship = self.ship;
        let red = a("systemEffectDamageReduction");
        let (mls, gunnery) = (ds.wk.skill_mls, ds.wk.skill_gunnery);
        let smartbomb = ds.groups.iter().find(|(_, g)| g.name == "Smart Bomb").map(|(k, _)| *k).unwrap_or(0);
        let n = self.items.len();
        for t in 0..n {
            let it = &self.items[t];
            if !it.owned || it.loc != Loc::Ship && it.kind != Kind::Drone {
                continue;
            }
            let mut dmg = false;
            let mut mult = false;
            match it.kind {
                Kind::Charge => dmg = it.req_skills.contains(&mls),
                Kind::Module => {
                    dmg = it.group == smartbomb;
                    mult = it.req_skills.contains(&gunnery);
                }
                Kind::Drone => mult = true,
                _ => {}
            }
            if dmg {
                for d in ["em", "thermal", "kinetic", "explosive"] {
                    self.push_mod(t, a(&format!("{d}Damage")), 6, Src::Attr { item: b, attr: red }, b, 6);
                }
            }
            if mult {
                self.push_mod(t, a("damageMultiplier"), 6, Src::Attr { item: b, attr: red }, b, 6);
            }
        }
        for d in ["Em", "Thermal", "Kinetic", "Explosive"] {
            for l in ["armor", "shield"] {
                self.push_mod(ship, a(&format!("{l}{d}DamageResonance")), 6, Src::Attr { item: b, attr: a(&format!("{l}{d}DamageResistanceBonus")) }, b, 6);
            }
        }
    }

    /// Pyfa's 'projected' handlers for remote reps, cap transfers and neuts/nos (eos/effects.py, LGPL).
    fn proj_special_for(&self, i: usize, name: &str, resist: u32) -> Option<Vec<ProjSpecial>> {
        let ds = self.ds;
        let a = |n: &str| ds.attr_id(n);
        let it = &self.items[i];
        let base = |n: &str| it.base_opt(ds.attr_id(n)).unwrap_or(0.0);
        let dist = it.distance;
        let falloff_factor = || crate::stats::range_factor(base("maxRange"), base("falloffEffectiveness"), dist, true);
        let gate = |opt: f64| if opt < dist.unwrap_or(0.0) { 0.0 } else { 1.0 };
        let no_assist = self.items[self.ship].base_opt(a("disallowAssistance")).map(|x| x != 0.0).unwrap_or(false);
        let rep = |layer: u8, amt: &str, mult: f64, factor: f64| {
            if no_assist { vec![] } else { vec![ProjSpecial::Rep { item: i, layer, amount: a(amt), mult, factor }] }
        };
        let drain = |amt: &str, dur: &str, factor: f64, sign: f64| vec![ProjSpecial::Drain { item: i, amount: a(amt), duration: a(dur), factor, resist, sign }];
        let no_offense = self.items[self.ship].base_opt(a("disallowOffensiveModifiers")).map(|x| x != 0.0).unwrap_or(false);
        let ecm = |fighter: bool, factor: f64| if no_offense { vec![] } else { vec![ProjSpecial::Ecm { item: i, fighter, factor, resist }] };
        let paste = it.charge.map(|c| ds.types.get(&self.items[c].type_id).map(|t| t.name == "Nanite Repair Paste").unwrap_or(false)).unwrap_or(false);
        Some(match name {
            "shipModuleRemoteShieldBooster" | "shipModuleAncillaryRemoteShieldBooster" => rep(0, "shieldBonus", 1.0, falloff_factor()),
            "shipModuleRemoteArmorRepairer" | "ShipModuleRemoteArmorMutadaptiveRepairer" => rep(1, "armorDamageAmount", 1.0, falloff_factor()),
            "shipModuleAncillaryRemoteArmorRepairer" => rep(1, "armorDamageAmount", if paste { 3.0 } else { 1.0 }, falloff_factor()),
            "shipModuleRemoteHullRepairer" => rep(2, "structureDamageAmount", 1.0, falloff_factor()),
            "npcEntityRemoteShieldBooster" => rep(0, "shieldBonus", 1.0, gate(base("maxRange"))),
            "npcEntityRemoteArmorRepairer" => rep(1, "armorDamageAmount", 1.0, gate(base("maxRange"))),
            "npcEntityRemoteHullRepairer" => rep(2, "structureDamageAmount", 1.0, gate(base("maxRange"))),
            "shipModuleRemoteCapacitorTransmitter" => {
                if no_assist { vec![] } else { drain("powerTransferAmount", "duration", gate(base("maxRange")), -1.0) }
            }
            "energyNeutralizerFalloff" => drain("energyNeutralizerAmount", "duration", falloff_factor(), 1.0),
            "fighterAbilityEnergyNeutralizer" => {
                let f = crate::stats::range_factor(base("fighterAbilityEnergyNeutralizerOptimalRange"), base("fighterAbilityEnergyNeutralizerFalloffRange"), dist, true);
                drain("fighterAbilityEnergyNeutralizerAmount", "fighterAbilityEnergyNeutralizerDuration", f * it.quantity.max(1) as f64, 1.0)
            }
            "remoteECMFalloff" | "structureModuleEffectECM" => ecm(false, falloff_factor()),
            "entityECMFalloff" => ecm(false, gate(base("ECMRangeOptimal"))),
            "ECMBurstJammer" => ecm(false, gate(base("ecmBurstRange"))),
            "fighterAbilityECM" => {
                let f = crate::stats::range_factor(base("fighterAbilityECMRangeOptimal"), base("fighterAbilityECMRangeFalloff"), dist, true);
                ecm(true, f * it.quantity.max(1) as f64)
            }
            "energyNosferatuFalloff" => drain("powerTransferAmount", "duration", falloff_factor(), 1.0),
            "structureEnergyNeutralizerFalloff" => drain("energyNeutralizerAmount", "duration", 1.0, 1.0),
            "entityEnergyNeutralizerFalloff" => {
                drain("energyNeutralizerAmount", "energyNeutralizerDuration", gate(base("energyNeutralizerRangeOptimal")), 1.0)
            }
            _ => return None,
        })
    }

    fn register_buffs(&mut self, req: &FitRequest) {
        let ds = self.ds;
        // aggregate explicit buffs per id according to the collection's aggregate mode
        let mut agg: FxHashMap<u32, f64> = FxHashMap::default();
        for b in &req.fleet.buffs {
            let Some(info) = ds.dbuffs.get(&b.buff_id) else {
                self.warnings.push(format!("unknown warfare buff {}", b.buff_id));
                continue;
            };
            let e = agg.entry(b.buff_id).or_insert(b.value);
            *e = match info.aggregate.as_deref() {
                Some("Minimum") => e.min(b.value),
                _ => e.max(b.value),
            };
        }
        // local command bursts (warfareBuffNID / warfareBuffNValue on active modules or their charges)
        let pairs: Vec<(u32, u32)> = (1..=4)
            .map(|k| (ds.attr_id(&format!("warfareBuff{k}ID")), ds.attr_id(&format!("warfareBuff{k}Value"))))
            .collect();
        // Pyfa keeps, per buff id, the single strongest (by |value|) source among the fit's own bursts and
        // the fleet booster fits; explicit `fleet.buffs` override both.
        let mut best: FxHashMap<u32, (f64, Src)> = FxHashMap::default();
        let offer = |best: &mut FxHashMap<u32, (f64, Src)>, id: u32, v: f64, src: Src| {
            match best.get(&id) {
                Some((old, _)) if old.abs() >= v.abs() => {}
                _ => {
                    best.insert(id, (v, src));
                }
            }
        };
        let n = self.items.len();
        for i in 0..n {
            if self.items[i].kind != Kind::Module || self.items[i].state < State::Active {
                continue;
            }
            // chargeBonusWarfareCharge PostAssigns warfareBuffNID onto the module and PostMuls the module's
            // warfareBuffNValue by the charge multiplier, so both are read (modified) from the module.
            for (ida, vala) in &pairs {
                let id = if self.has(i, *ida) { self.get(i, *ida) as u32 } else { 0 };
                if id == 0 || agg.contains_key(&id) {
                    continue;
                }
                let v = self.get(i, *vala);
                offer(&mut best, id, v, Src::Attr { item: i, attr: *vala });
            }
        }
        // abyssal weather / AoE cloud beacons (Pyfa weather_* / aoe_beacon_* effects): warfareBuff1/2 of the
        // environment item join the same command-bonus pool (strongest |value| per buff id)
        for i in 0..n {
            if self.items[i].kind != Kind::Beacon {
                continue;
            }
            let weather = self.items[i].effects.iter().any(|(e, _)| {
                ds.effects.get(e).map_or(false, |ei| ei.name.starts_with("weather_") || ei.name.starts_with("aoe_beacon_"))
            });
            if !weather {
                continue;
            }
            for (ida, vala) in &pairs[..2] {
                let id = if self.has(i, *ida) { self.get(i, *ida) as u32 } else { 0 };
                if id == 0 || agg.contains_key(&id) {
                    continue;
                }
                let v = self.get(i, *vala);
                offer(&mut best, id, v, Src::Const(v));
            }
        }
        for (k, bf) in req.fleet.booster_fits.iter().enumerate() {
            let mut breq = bf.clone();
            breq.fleet.booster_fits.clear();
            match Fit::build(ds, &breq) {
                Ok(b) => {
                    for i in 0..b.items.len() {
                        if b.items[i].kind != Kind::Module || b.items[i].state < State::Active {
                            continue;
                        }
                        for (ida, vala) in &pairs {
                            let id = if b.has(i, *ida) { b.get(i, *ida) as u32 } else { 0 };
                            if id == 0 || agg.contains_key(&id) {
                                continue;
                            }
                            let v = b.get(i, *vala);
                            offer(&mut best, id, v, Src::Const(v));
                        }
                    }
                }
                Err(e) => self.warnings.push(format!("fleet.booster_fits[{k}]: {e:?}")),
            }
        }
        for (id, value) in agg.iter() {
            best.insert(*id, (*value, Src::Const(*value)));
        }
        let mut ids: Vec<_> = best.into_iter().collect();
        ids.sort_by_key(|x| x.0);
        for (id, (_, src)) in ids {
            let target = match src {
                Src::Attr { item, .. } => item,
                _ => self.ship,
            };
            self.apply_buff(id, src, target);
        }
        self.clear_cache();
    }

    fn clear_cache(&self) {
        for it in &self.items {
            for a in it.attrs.values() {
                a.val.set(None);
            }
        }
    }

    /// Reactive Armor Hardener adaptation (no modifierInfo in the SDE). Simulates RAH cycles against the
    /// incoming damage pattern (after the ship's other armor resists) until it loops, averages the loop and
    /// applies the averaged resonances as a stacking-penalised PreMul - same algorithm as Pyfa/eos (LGPL).
    /// `options.rah = "disable"` applies the module's unadapted resonances instead.
    fn apply_rah(&mut self, req: &FitRequest) {
        let ds = self.ds;
        let eid = ds.effect_id("adaptiveArmorHardener");
        if eid == 0 {
            return;
        }
        let names = ["armorEmDamageResonance", "armorThermalDamageResonance", "armorKineticDamageResonance", "armorExplosiveDamageResonance"];
        let attrs: Vec<u32> = names.iter().map(|n| ds.attr_id(n)).collect();
        let shift_attr = ds.attr_id("resistanceShiftAmount");
        let rahs: Vec<usize> = (0..self.items.len())
            .filter(|&i| {
                self.items[i].kind == Kind::Module && self.items[i].state >= State::Active && self.items[i].effects.iter().any(|(e, _)| *e == eid)
            })
            .collect();
        let disable = req.options.rah.as_deref() == Some("disable");
        let dp = req.damage_pattern.unwrap_or(crate::request::Resists { em: 25.0, thermal: 25.0, kinetic: 25.0, explosive: 25.0 });
        let pattern = [dp.em, dp.thermal, dp.kinetic, dp.explosive];
        let ship = self.ship;
        for m in rahs {
            self.clear_cache();
            let mut res: Vec<f64> = attrs.iter().map(|&a| self.get(m, a)).collect();
            if !disable {
                let base: Vec<f64> = (0..4).map(|k| pattern[k] * self.get(ship, attrs[k])).collect();
                let shift = self.get(m, shift_attr) / 100.0;
                let mut cycles: Vec<[f64; 4]> = Vec::new();
                let mut loop_start: isize = -20;
                for _ in 0..50 {
                    // in-game tie order em, explosive, kinetic, thermal
                    let mut t: Vec<(usize, f64, f64)> = [0usize, 3, 2, 1].iter().map(|&k| (k, base[k] * res[k], res[k])).collect();
                    t.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)); // stable like Python
                    let (c0, c1, c2, c3);
                    if t[2].1 == 0.0 {
                        c0 = 1.0 - t[0].2;
                        c1 = 1.0 - t[1].2;
                        c2 = 1.0 - t[2].2;
                        c3 = -(c0 + c1 + c2);
                    } else if t[1].1 == 0.0 {
                        c0 = 1.0 - t[0].2;
                        c1 = 1.0 - t[1].2;
                        c2 = -(c0 + c1) / 2.0;
                        c3 = c2;
                    } else {
                        c0 = shift.min(1.0 - t[0].2);
                        c1 = shift.min(1.0 - t[1].2);
                        c2 = -(c0 + c1) / 2.0;
                        c3 = c2;
                    }
                    res[t[0].0] = t[0].2 + c0;
                    res[t[1].0] = t[1].2 + c1;
                    res[t[2].0] = t[2].2 + c2;
                    res[t[3].0] = t[3].2 + c3;
                    if let Some(i) = cycles.iter().position(|v| (0..4).all(|k| (res[k] - v[k]).abs() <= 1e-6)) {
                        loop_start = i as isize;
                        break;
                    }
                    cycles.push([res[0], res[1], res[2], res[3]]);
                }
                let start = if loop_start >= 0 { loop_start as usize } else { cycles.len().saturating_sub(20) };
                let lp = &cycles[start..];
                if !lp.is_empty() {
                    for k in 0..4 {
                        res[k] = ((lp.iter().map(|v| v[k]).sum::<f64>() / lp.len() as f64) * 1000.0).round() / 1000.0;
                    }
                }
            }
            let cat = self.items[m].category;
            for k in 0..4 {
                if !disable {
                    self.push_mod(m, attrs[k], 7, Src::Const(res[k]), m, cat);
                }
                self.push_mod(ship, attrs[k], 0, Src::Const(res[k]), m, cat);
            }
        }
        self.clear_cache();
    }

    fn apply_buff(&mut self, id: u32, src: Src, source_item: usize) {
        let ds = self.ds;
        let Some(info) = ds.dbuffs.get(&id) else { return };
        let op = info.op;
        let ship = self.ship;
        // Pyfa applies most buffs stacking-penalised; the abyssal weather resistance/HP/velocity buffs are not
        let cat = if matches!(id, 90 | 93 | 94 | 95 | 96 | 98 | 99) { 6 } else { 0 };
        for a in info.item.clone() {
            self.push_mod(ship, a, op, src, source_item, cat);
        }
        // AoE cloud / weather buffs also hit drones that require the Drones skill (Pyfa fit.py commandBonus)
        let drone_attrs: &[&str] = match id {
            79 => &["signatureRadius"],
            90 => &["shieldEmDamageResonance", "armorEmDamageResonance", "emDamageResonance"],
            93 => &["shieldExplosiveDamageResonance", "armorExplosiveDamageResonance", "explosiveDamageResonance"],
            95 => &["shieldThermalDamageResonance", "armorThermalDamageResonance", "thermalDamageResonance"],
            99 => &["shieldKineticDamageResonance", "armorKineticDamageResonance", "kineticDamageResonance"],
            94 => &["shieldCapacity"],
            96 => &["armorHP"],
            97 => &["maxRange", "falloff"],
            98 => &["maxVelocity"],
            _ => &[],
        };
        if !drone_attrs.is_empty() {
            let drones_skill = 3436;
            let drones: Vec<usize> = (0..self.items.len())
                .filter(|&d| self.items[d].kind == Kind::Drone && self.items[d].req_skills.contains(&drones_skill))
                .collect();
            for d in drones {
                for n in drone_attrs {
                    let a = ds.attr_id(n);
                    if a != 0 {
                        self.push_mod(d, a, op, src, source_item, cat);
                    }
                }
            }
        }
        for a in info.location.clone() {
            for t in self.targets(ship, Func::Location, Domain::Ship, 0) {
                self.push_mod(t, a, op, src, source_item, cat);
            }
        }
        for (a, g) in info.location_group.clone() {
            for t in self.targets(ship, Func::LocationGroup, Domain::Ship, g) {
                self.push_mod(t, a, op, src, source_item, cat);
            }
        }
        for (a, s) in info.location_skill.clone() {
            for t in self.targets(ship, Func::LocationRequiredSkill, Domain::Ship, s) {
                self.push_mod(t, a, op, src, source_item, cat);
            }
        }
    }

    // ---------------------------------------------------------------- evaluation
    pub fn get(&self, item: usize, attr: u32) -> f64 {
        let it = &self.items[item];
        match it.attrs.get(&attr) {
            Some(a) => self.eval(item, attr, a),
            None => match it.tbase(attr) {
                Some(b) => self.finish(item, attr, b),
                None => self.ds.attr_default(attr),
            },
        }
    }

    pub fn get_opt(&self, item: usize, attr: u32) -> Option<f64> {
        let it = &self.items[item];
        match it.attrs.get(&attr) {
            Some(a) => Some(self.eval(item, attr, a)),
            None => it.tbase(attr).map(|b| self.finish(item, attr, b)),
        }
    }

    pub fn has(&self, item: usize, attr: u32) -> bool {
        self.items[item].has_attr(attr)
    }

    pub fn base(&self, item: usize, attr: u32) -> f64 {
        self.items[item].base_opt(attr).unwrap_or_else(|| self.ds.attr_default(attr))
    }

    /// value of an unmodified attribute: base with the attribute's min/max caps and fitting rounding
    fn finish(&self, item: usize, attr_id: u32, base: f64) -> f64 {
        let Some(info) = self.ds.attrs.get(&attr_id) else { return base };
        let mut val = base;
        if let Some(mn) = info.min_attr {
            val = val.max(self.get(item, mn));
        }
        if let Some(mx) = info.max_attr {
            val = val.min(self.get(item, mx));
        }
        if info.round2 {
            val = crate::stats::py_round2(val);
        }
        val
    }

    fn src_value(&self, s: &Src) -> f64 {
        match *s {
            Src::Attr { item, attr } => {
                if self.items[item].kind == Kind::Module
                    && self.ds.attrs.get(&attr).map_or(false, |i| i.overload)
                {
                    self.eval_before(item, attr)
                } else {
                    self.get(item, attr)
                }
            }
            Src::Const(v) => v,
            Src::Prop { module, ship, speed, thrust, mass } => {
                let m = self.get(ship, mass);
                if m == 0.0 {
                    1.0
                } else {
                    1.0 + self.get(module, speed) / 100.0 * self.get(module, thrust) / m
                }
            }
            Src::Projected { item, attr, factor, target, resist, mul } => {
                let mut f = factor;
                if resist != 0 {
                    f *= self.get(target, resist);
                }
                let v = self.get(item, attr);
                if mul { (v - 1.0) * f + 1.0 } else { v * f }
            }
        }
    }

    fn eval(&self, item: usize, attr_id: u32, a: &Attr) -> f64 {
        if let Some(v) = a.val.get() {
            return v;
        }
        if a.busy.get() {
            return a.base; // cycle guard
        }
        a.busy.set(true);
        let val = self.combine(item, attr_id, a, None);
        a.busy.set(false);
        a.val.set(Some(val));
        val
    }

    /// Pyfa runs effects item by item in fit order: an overheat effect reads its module's `overload*`
    /// attribute before modules listed later in the fit have applied their modifiers (e.g. a Tengu defensive
    /// subsystem listed after the shield hardener has not boosted overloadHardeningBonus yet).
    fn eval_before(&self, item: usize, attr_id: u32) -> f64 {
        let Some(a) = self.items[item].attrs.get(&attr_id) else { return self.get(item, attr_id) };
        let Some(lim) = self.items[item].req_index else { return self.get(item, attr_id) };
        let later = |m: &AMod| {
            let s = &self.items[m.source_item];
            s.kind == Kind::Module && s.loc == Loc::Ship && s.req_index.map_or(false, |r| r > lim)
        };
        if !a.mods.iter().any(later) {
            return self.get(item, attr_id);
        }
        if a.busy.get() {
            return a.base;
        }
        a.busy.set(true);
        let v = self.combine(item, attr_id, a, Some(lim));
        a.busy.set(false);
        v
    }

    fn combine(&self, item: usize, attr_id: u32, a: &Attr, before: Option<usize>) -> f64 {
        let info = self.ds.attrs.get(&attr_id);
        let mut val = a.base;
        if !a.mods.is_empty() {
            let mut vals: smallvec::SmallVec<[(i32, bool, f64); 8]> = smallvec::SmallVec::with_capacity(a.mods.len());
            for m in &a.mods {
                if let Some(lim) = before {
                    let s = &self.items[m.source_item];
                    if s.kind == Kind::Module && s.loc == Loc::Ship && s.req_index.map_or(false, |r| r > lim) {
                        continue;
                    }
                }
                vals.push((m.op, m.penalized, self.src_value(&m.src)));
            }
            for op in [-1, 0, 1, 2, 3, 4, 5, 6, 7] {
                let mut any = false;
                let mut pos: Vec<f64> = Vec::new();
                let mut neg: Vec<f64> = Vec::new();
                let mut assign: Option<f64> = None;
                for &(o, pen, v) in &vals {
                    if o != op {
                        continue;
                    }
                    any = true;
                    match op {
                        -1 | 7 => {
                            let hig = info.map(|i| i.high_is_good).unwrap_or(true);
                            assign = Some(match assign {
                                None => v,
                                Some(c) => {
                                    if hig {
                                        c.max(v)
                                    } else {
                                        c.min(v)
                                    }
                                }
                            });
                        }
                        2 => val += v,
                        3 => val -= v,
                        _ => {
                            let m = match op {
                                0 | 4 => v,
                                1 | 5 => {
                                    if v == 0.0 {
                                        1.0
                                    } else {
                                        1.0 / v
                                    }
                                }
                                6 => 1.0 + v / 100.0,
                                _ => 1.0,
                            };
                            if pen {
                                if m > 1.0 {
                                    pos.push(m)
                                } else if m < 1.0 {
                                    neg.push(m)
                                }
                            } else {
                                val *= m;
                            }
                        }
                    }
                }
                if !any {
                    continue;
                }
                if let Some(v) = assign {
                    val = v;
                }
                for list in [&mut pos, &mut neg] {
                    list.sort_by(|x, y| (y - 1.0).abs().partial_cmp(&(x - 1.0).abs()).unwrap_or(std::cmp::Ordering::Equal));
                    for (i, m) in list.iter().enumerate() {
                        val *= 1.0 + (m - 1.0) * stack_factor(i);
                    }
                }
            }
        }
        if let Some(info) = info {
            if let Some(mn) = info.min_attr {
                val = val.max(self.get(item, mn));
            }
            if let Some(mx) = info.max_attr {
                val = val.min(self.get(item, mx));
            }
            if info.round2 {
                val = crate::stats::py_round2(val);
            }
        }
        val
    }
}

fn set_type_attrs(_item: &mut Item<'_>, _t: &TypeInfo) {
    // base attributes (incl. the authoritative type-level mass/capacity/volume/radius merged at dataset
    // load) stay in the dataset: item.tattrs
}

impl Item<'_> {
    /// dataset base value of an attribute of this item's type
    #[inline]
    pub fn tbase(&self, attr: u32) -> Option<f64> {
        self.tattrs.binary_search_by_key(&attr, |x| x.0).ok().map(|i| self.tattrs[i].1)
    }
    /// unmodified base value (override/mutation or dataset), None if the type lacks the attribute
    #[inline]
    pub fn base_opt(&self, attr: u32) -> Option<f64> {
        match self.attrs.get(&attr) {
            Some(a) => Some(a.base),
            None => self.tbase(attr),
        }
    }
    pub fn has_attr(&self, attr: u32) -> bool {
        self.attrs.contains_key(&attr) || self.tbase(attr).is_some()
    }
    /// all attribute ids present on the item (sorted)
    pub fn attr_ids(&self) -> Vec<u32> {
        let mut k: Vec<u32> = self.tattrs.iter().map(|x| x.0).chain(self.attrs.keys().copied()).collect();
        k.sort_unstable();
        k.dedup();
        k
    }
}

/// Slot from the type's slot effect (hiPower 12, medPower 13, loPower 11, rigSlot 2663, subSystem 3772, serviceSlot 6306).
pub fn infer_slot(_ds: &Dataset, t: &TypeInfo) -> Option<Slot> {
    for (e, _) in &t.effects {
        match *e {
            12 => return Some(Slot::High),
            13 => return Some(Slot::Mid),
            11 => return Some(Slot::Low),
            2663 => return Some(Slot::Rig),
            3772 => return Some(Slot::Subsystem),
            6306 => return Some(Slot::Service),
            _ => {}
        }
    }
    None
}
