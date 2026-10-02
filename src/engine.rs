//! Data-driven dogma engine: builds the object graph for one request, registers modifiers, evaluates lazily.
use crate::data::{Dataset, Domain, Func, TypeInfo};
use crate::request::{FitRequest, ModuleReq, Slot, State};
use rustc_hash::FxHashMap;
use std::cell::Cell;

/// Source categories exempt from stacking penalties: Ship, Charge, Skill, Implant, Subsystem, Structure.
const EXEMPT_CATEGORIES: [u32; 6] = [6, 8, 16, 20, 32, 65];
/// requiredSkill1..6
pub const REQ_SKILL_ATTRS: [u32; 6] = [182, 183, 184, 1285, 1289, 1290];
pub const ATTR_SKILL_LEVEL: u32 = 280;
const EFFECT_SKILL_EFFECT: u32 = 132;
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
    pub mods: Vec<AMod>,
    val: Cell<Option<f64>>,
    busy: Cell<bool>,
}

impl Attr {
    fn new(base: f64) -> Attr {
        Attr { base, mods: Vec::new(), val: Cell::new(None), busy: Cell::new(false) }
    }
}

#[derive(Debug)]
pub struct Item {
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
    pub attrs: FxHashMap<u32, Attr>,
    pub req_skills: Vec<u32>,
    /// effect ids carried by this item (own + mutation base)
    pub effects: Vec<(u32, bool)>,
    pub fighter_abilities: Option<Vec<u32>>,
    pub booster_side_effects: Vec<u32>,
    pub spool: Option<crate::request::Spool>,
    pub distance: Option<f64>,
}

pub struct Fit<'a> {
    pub ds: &'a Dataset,
    pub items: Vec<Item>,
    pub ship: usize,
    pub char: usize,
    pub warnings: Vec<String>,
    pub is_structure: bool,
}

#[derive(Debug)]
pub struct EngineError {
    pub code: &'static str,
    pub message: String,
    pub path: String,
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
    fn new_item(&mut self, type_id: u32, kind: Kind, loc: Loc, path: &str) -> Result<usize, EngineError> {
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
            attrs: FxHashMap::default(),
            req_skills: Vec::new(),
            effects: t.effects.clone(),
            fighter_abilities: None,
            booster_side_effects: Vec::new(),
            spool: None,
            distance: None,
        };
        set_type_attrs(&mut item, t);
        item.req_skills = REQ_SKILL_ATTRS
            .iter()
            .filter_map(|a| t.attr(*a))
            .map(|v| v as u32)
            .filter(|v| *v != 0)
            .collect();
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
                    item.effects.push((*e, *d));
                }
            }
            if item.req_skills.is_empty() {
                item.req_skills = REQ_SKILL_ATTRS
                    .iter()
                    .filter_map(|a| base.attr(*a))
                    .map(|v| v as u32)
                    .filter(|v| *v != 0)
                    .collect();
            }
            if item.attrs.get(&4).map(|a| a.base).unwrap_or(0.0) == 0.0 && base.mass != 0.0 {
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

    fn add_module(&mut self, i: usize, m: &ModuleReq, path: &str) -> Result<usize, EngineError> {
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
            let cidx = self.new_item(c, Kind::Charge, Loc::Ship, &format!("{path}/charge_type_id"))?;
            self.items[cidx].parent = Some(idx);
            self.items[cidx].req_index = Some(i);
            self.items[idx].charge = Some(cidx);
        }
        Ok(idx)
    }

    /// Build the object graph for a request. Does not evaluate anything.
    pub fn build(ds: &'a Dataset, req: &FitRequest) -> Result<Fit<'a>, EngineError> {
        let mut fit = Fit { ds, items: Vec::with_capacity(512), ship: 0, char: 0, warnings: Vec::new(), is_structure: false };
        let ship = fit.new_item(req.ship.type_id, Kind::Ship, Loc::Ship, "/ship/type_id")?;
        fit.ship = ship;
        fit.is_structure = fit.items[ship].category == 65;
        let ch = fit.new_item(1373, Kind::Char, Loc::Char, "/character")?;
        fit.char = ch;
        if let Some(sec) = req.character.security_status {
            let a = ds.attr_id("pilotSecurityStatus");
            if a != 0 {
                fit.items[ch].attrs.insert(a, Attr::new(sec));
            }
        }
        // skills
        let default_level = req.character.skills.default_level.unwrap_or(0);
        let mut levels: FxHashMap<u32, u8> = FxHashMap::default();
        // every published skill exists (untrained = level 0): ship-bonus attrs like shipBonusGC2 are
        // scaled by a skill-level PreMul on the skill, so a missing skill would leave the raw per-level value
        for s in &ds.skills {
            if ds.types[s].published {
                levels.insert(*s, default_level);
            }
        }
        for (k, v) in &req.character.skills.levels {
            if let Ok(id) = k.parse::<u32>() {
                levels.insert(id, *v);
            } else if let Some(id) = ds.type_by_name(k) {
                levels.insert(id, *v);
            }
        }
        let mut lv: Vec<(u32, u8)> = levels.into_iter().collect();
        lv.sort();
        for (s, l) in lv {
            if !ds.types.contains_key(&s) {
                continue;
            }
            let idx = fit.new_item(s, Kind::Skill, Loc::Char, "/character/skills")?;
            fit.items[idx].attrs.insert(ATTR_SKILL_LEVEL, Attr::new(l.min(5) as f64));
            fit.items[idx].owned = false;
        }
        // Tactical destroyers must have a mode: default to the first (lowest type id) like Pyfa / the client.
        let mode_id = req.ship.mode_type_id.or_else(|| {
            let ship_name = ds.types.get(&req.ship.type_id)?.name.to_lowercase();
            let m = ds
                .types
                .iter()
                .filter(|(_, t)| t.group == 1306 && t.name.to_lowercase().starts_with(&ship_name))
                .map(|(id, _)| *id)
                .min()?;
            fit.warnings.push(format!("no tactical mode given; defaulted to type {m}"));
            Some(m)
        });
        if let Some(mode) = mode_id {
            let idx = fit.new_item(mode, Kind::Mode, Loc::Nowhere, "/ship/mode_type_id")?;
            fit.items[idx].owned = false;
        }
        for (i, m) in req.modules.iter().enumerate() {
            fit.add_module(i, m, &format!("/modules/{i}"))?;
        }
        for (i, d) in req.drones.iter().enumerate() {
            let idx = fit.new_item(d.type_id, Kind::Drone, Loc::Space, &format!("/drones/{i}"))?;
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
            let idx = fit.new_item(f.type_id, Kind::Fighter, Loc::Space, &format!("/fighters/{i}"))?;
            let sq = ds.attr_id("fighterSquadronMaxSize");
            let maxsq = fit.items[idx].attrs.get(&sq).map(|a| a.base as u32).unwrap_or(1);
            let it = &mut fit.items[idx];
            it.quantity = f.quantity.unwrap_or(maxsq).clamp(1, maxsq.max(1));
            if f.quantity.unwrap_or(0) > maxsq {
                fit.warnings.push(format!("fighters/{i}: squadron size {} capped to {maxsq}", f.quantity.unwrap_or(0)));
            }
            it.active_count = if f.active { it.quantity } else { 0 };
            it.state = if f.active { State::Active } else { State::Offline };
            it.fighter_abilities = f.abilities.clone().or_else(|| {
                // Pyfa default: standard attack on; other abilities (except MWD/evasive/MJD) on only if they
                // come before the standard attack in effect order
                let mut ids: Vec<u32> = it.effects.iter().map(|(e, _)| *e).collect();
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
                    } else if !std_seen
                        && !matches!(n, "fighterAbilityMicroWarpDrive" | "fighterAbilityEvasiveManeuvers" | "fighterAbilityMicroJumpDrive")
                    {
                        on.push(e);
                    }
                }
                Some(on)
            });
            it.req_index = Some(i);
        }
        for (i, imp) in req.implants.iter().enumerate() {
            let idx = fit.new_item(*imp, Kind::Implant, Loc::Char, &format!("/implants/{i}"))?;
            fit.items[idx].owned = false;
            fit.items[idx].req_index = Some(i);
        }
        for (i, b) in req.boosters.iter().enumerate() {
            let idx = fit.new_item(b.type_id, Kind::Booster, Loc::Char, &format!("/boosters/{i}"))?;
            fit.items[idx].owned = false;
            fit.items[idx].booster_side_effects = b.side_effects.clone();
            fit.items[idx].req_index = Some(i);
        }
        for (i, e) in req.environment.effect_type_ids.iter().enumerate() {
            let idx = fit.new_item(*e, Kind::Beacon, Loc::Nowhere, &format!("/environment/effect_type_ids/{i}"))?;
            fit.items[idx].owned = false;
        }
        for (i, p) in req.projected.iter().enumerate() {
            match p.kind.as_str() {
                "module" => {
                    if let Some(m) = &p.module {
                        for _ in 0..p.amount.max(1) {
                            let idx = fit.new_item(m.type_id, Kind::Projected, Loc::Nowhere, &format!("/projected/{i}"))?;
                            let it = &mut fit.items[idx];
                            it.owned = false;
                            it.state = m.state.unwrap_or(State::Active);
                            it.distance = p.distance_m;
                            it.req_index = Some(i);
                        }
                    }
                }
                "drone" => {
                    if let Some(d) = &p.drone {
                        for _ in 0..(p.amount.max(1) * d.quantity.max(1)) {
                            let idx = fit.new_item(d.type_id, Kind::Projected, Loc::Nowhere, &format!("/projected/{i}"))?;
                            let it = &mut fit.items[idx];
                            it.owned = false;
                            it.state = State::Active;
                            it.distance = p.distance_m;
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
                if let Some(v) = it.attrs.get(&src_id).map(|a| a.base) {
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
        let stackable = ds.attrs.get(&attr).map(|a| a.stackable).unwrap_or(true);
        let penalized = !stackable && !EXEMPT_CATEGORIES.contains(&source_cat);
        let def = ds.attr_default(attr);
        let a = self.items[target].attrs.entry(attr).or_insert_with(|| Attr::new(def));
        a.mods.push(AMod { op, penalized, src, source_item });
    }

    fn targets(&self, src: usize, func: Func, domain: Domain, extra: u32) -> Vec<usize> {
        let items = &self.items;
        let s = &items[src];
        let mut out = Vec::new();
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
                    return out;
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
        out
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
            for (eid, is_default) in effects {
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
                if !state_ok(e.category, state) {
                    continue;
                }
                // ---- special effects (no modifierInfo in the SDE)
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
                    let targets = self.targets(i, m.func, m.domain, extra);
                    // Bastion hull resists are not stacking penalised in game (observed by Pyfa); SDE marks the attrs non-stackable
                    let cat = if eid == e_bastion && HULL_RESONANCES.contains(&m.modified) { 6 } else { src_cat };
                    for t in targets {
                        self.push_mod(t, m.modified, m.op, Src::Attr { item: i, attr: m.modifying }, i, cat);
                    }
                }
            }
        }
        self.register_buffs(req);
    }

    fn register_projected(&mut self, i: usize) {
        let ds = self.ds;
        let src_cat = self.items[i].category;
        let state = self.items[i].state;
        let effects = self.items[i].effects.clone();
        let ship = self.ship;
        for (eid, _) in effects {
            let Some(e) = ds.effects.get(&eid) else { continue };
            if e.category != 2 && e.category != 3 {
                continue;
            }
            if state < State::Active {
                continue;
            }
            let factor = {
                let it = &self.items[i];
                let opt = e.range_attr.and_then(|a| it.attrs.get(&a)).map(|a| a.base).unwrap_or(0.0);
                let fo = e.falloff_attr.and_then(|a| it.attrs.get(&a)).map(|a| a.base).unwrap_or(0.0);
                crate::stats::range_factor(opt, fo, it.distance, true)
            };
            let resist = e.resistance_attr.unwrap_or_else(|| {
                self.items[i].attrs.get(&ds.attr_id("remoteResistanceID")).map(|a| a.base as u32).unwrap_or(0)
            });
            let mut push = |fit: &mut Fit, target_attr: u32, src_attr: u32, op: i32| {
                let mul = op == 4 || op == 0;
                fit.push_mod(
                    ship,
                    target_attr,
                    op,
                    Src::Projected { item: i, attr: src_attr, factor, target: ship, resist, mul },
                    i,
                    src_cat,
                );
            };
            if !e.mods.is_empty() {
                for m in &e.mods {
                    if matches!(m.domain, Domain::TargetId | Domain::Target | Domain::Ship) && m.func == Func::Item {
                        push(self, m.modified, m.modifying, m.op);
                    }
                }
                continue;
            }
            let name = e.name.as_str();
            if name.starts_with("remoteWebifier") || name == "structureModuleEffectStasisWebifier" {
                push(self, ds.attr_id("maxVelocity"), ds.attr_id("speedFactor"), 6);
            } else if name.starts_with("remoteTargetPaint") || name == "structureModuleEffectTargetPainter" {
                push(self, ds.attr_id("signatureRadius"), ds.attr_id("signatureRadiusBonus"), 6);
            } else if name.starts_with("remoteSensorDamp") || name == "structureModuleEffectRemoteSensorDampener" {
                push(self, ds.attr_id("maxTargetRange"), ds.attr_id("maxTargetRangeBonus"), 6);
                push(self, ds.attr_id("scanResolution"), ds.attr_id("scanResolutionBonus"), 6);
            } else if name.starts_with("remoteSensorBoost") {
                push(self, ds.attr_id("maxTargetRange"), ds.attr_id("maxTargetRangeBonus"), 6);
                push(self, ds.attr_id("scanResolution"), ds.attr_id("scanResolutionBonus"), 6);
            } else {
                self.warnings.push(format!("projected effect '{name}' not modelled yet"));
            }
        }
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
        let mut ids: Vec<_> = agg.into_iter().collect();
        ids.sort_by_key(|x| x.0);
        for (id, value) in ids {
            self.apply_buff(id, Src::Const(value), self.ship);
        }
        // local command bursts (warfareBuffNID / warfareBuffNValue on active modules or their charges)
        let pairs: Vec<(u32, u32)> = (1..=4)
            .map(|k| (ds.attr_id(&format!("warfareBuff{k}ID")), ds.attr_id(&format!("warfareBuff{k}Value"))))
            .collect();
        let explicit: Vec<u32> = req.fleet.buffs.iter().map(|b| b.buff_id).collect();
        let n = self.items.len();
        for i in 0..n {
            if self.items[i].kind != Kind::Module || self.items[i].state < State::Active {
                continue;
            }
            // chargeBonusWarfareCharge PostAssigns warfareBuffNID onto the module and PostMuls the module's
            // warfareBuffNValue by the charge multiplier, so both are read (modified) from the module.
            let src_item = i;
            for (ida, vala) in &pairs {
                let id = if self.has(i, *ida) { self.get(i, *ida) as u32 } else { 0 };
                if id == 0 || explicit.contains(&id) {
                    continue;
                }
                self.apply_buff(id, Src::Attr { item: src_item, attr: *vala }, i);
            }
        }
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
        let cat = 0; // buffs are never exempt
        for a in info.item.clone() {
            self.push_mod(ship, a, op, src, source_item, cat);
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
        match self.items[item].attrs.get(&attr) {
            Some(a) => self.eval(item, attr, a),
            None => self.ds.attr_default(attr),
        }
    }

    pub fn get_opt(&self, item: usize, attr: u32) -> Option<f64> {
        self.items[item].attrs.get(&attr).map(|a| self.eval(item, attr, a))
    }

    pub fn has(&self, item: usize, attr: u32) -> bool {
        self.items[item].attrs.contains_key(&attr)
    }

    pub fn base(&self, item: usize, attr: u32) -> f64 {
        self.items[item].attrs.get(&attr).map(|a| a.base).unwrap_or_else(|| self.ds.attr_default(attr))
    }

    fn src_value(&self, s: &Src) -> f64 {
        match *s {
            Src::Attr { item, attr } => self.get(item, attr),
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
        let info = self.ds.attrs.get(&attr_id);
        let mut val = a.base;
        if !a.mods.is_empty() {
            let mut vals: Vec<(i32, bool, f64)> = Vec::with_capacity(a.mods.len());
            for m in &a.mods {
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
                        val *= 1.0 + (m - 1.0) * (-((i * i) as f64) / 7.1289).exp();
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
            if matches!(info.name.as_str(), "cpu" | "power" | "cpuOutput" | "powerOutput") {
                val = (val * 100.0).round() / 100.0;
            }
        }
        a.busy.set(false);
        a.val.set(Some(val));
        val
    }
}

fn set_type_attrs(item: &mut Item, t: &TypeInfo) {
    for (a, v) in &t.attrs {
        item.attrs.insert(*a, Attr::new(*v));
    }
    // type-level fields are authoritative (mass/capacity/volume/radius)
    for (a, v) in [(4u32, t.mass), (38, t.capacity), (161, t.volume), (162, t.radius)] {
        if v != 0.0 || !item.attrs.contains_key(&a) {
            item.attrs.insert(a, Attr::new(v));
        }
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
