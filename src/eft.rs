//! Minimal EFT text import/export (format: public EVE fitting text, written clean-room).
use crate::data::Dataset;
use crate::engine::infer_slot;
use crate::request::*;

/// strip a trailing " [N]" mutation reference
fn mut_ref(line: &str) -> (&str, Option<u32>) {
    let l = line.trim_end();
    if l.ends_with(']') {
        if let Some(p) = l.rfind(" [") {
            if let Ok(n) = l[p + 2..l.len() - 1].parse::<u32>() {
                return (l[..p].trim_end(), Some(n));
            }
        }
    }
    (l, None)
}

/// Parse the trailing mutation blocks:  "[N] Base Name" / "  Mutaplasmid Name" / "  attr value, attr value"
fn parse_mutations(ds: &Dataset, text: &str) -> Result<(std::collections::HashMap<u32, Mutation>, usize), String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = std::collections::HashMap::new();
    let is_head = |l: &str| {
        let t = l.trim();
        t.starts_with('[') && t.find(']').map(|e| t[1..e].parse::<u32>().is_ok()).unwrap_or(false)
    };
    let first = lines.iter().position(|l| is_head(l)).unwrap_or(lines.len());
    let mut i = first;
    while i < lines.len() {
        let t = lines[i].trim();
        if !is_head(t) {
            i += 1;
            continue;
        }
        let e = t.find(']').unwrap();
        let n: u32 = t[1..e].parse().unwrap();
        let base_name = t[e + 1..].trim();
        let base = ds.type_by_name(base_name).ok_or(format!("unknown mutated base '{base_name}'"))?;
        let mut m = Mutation { base_type_id: base, mutaplasmid_type_id: None, attributes: Default::default() };
        i += 1;
        while i < lines.len() && !is_head(lines[i]) {
            let l = lines[i].trim();
            i += 1;
            if l.is_empty() {
                continue;
            }
            if m.mutaplasmid_type_id.is_none() {
                m.mutaplasmid_type_id = Some(ds.type_by_name(l).ok_or(format!("unknown mutaplasmid '{l}'"))?);
                continue;
            }
            for kv in l.split(',') {
                let kv = kv.trim();
                if let Some((k, v)) = kv.rsplit_once(' ') {
                    let aid = ds.attr_id(k.trim());
                    if aid != 0 {
                        if let Ok(v) = v.trim().parse::<f64>() {
                            m.attributes.insert(aid.to_string(), v);
                        }
                    }
                }
            }
        }
        out.insert(n, m);
    }
    Ok((out, first))
}

/// resulting (mutated) type id for base + mutaplasmid
fn mutated_type(ds: &Dataset, m: &Mutation) -> u32 {
    m.mutaplasmid_type_id
        .and_then(|id| ds.mutaplasmids.get(&id))
        .and_then(|mu| mu.mapping.iter().find(|x| x.inputs.contains(&m.base_type_id)).map(|x| x.output))
        .unwrap_or(m.base_type_id)
}

pub fn parse(ds: &Dataset, text: &str) -> Result<FitRequest, String> {
    let (muts, first_mut_line) = parse_mutations(ds, text)?;
    let body: Vec<&str> = text.lines().take(first_mut_line).collect();
    let mut lines = body.iter().map(|l| l.trim()).filter(|l| !l.is_empty());
    let header = lines.next().ok_or("empty EFT")?;
    let h = header.trim_start_matches('[').trim_end_matches(']');
    let ship_name = h.split(',').next().unwrap_or("").trim();
    let ship = ds.type_by_name(ship_name).ok_or(format!("unknown ship '{ship_name}'"))?;
    let mut req = FitRequest {
        schema_version: Some(1),
        ship: ShipReq { type_id: ship, mode_type_id: None },
        character: Character::default(),
        modules: vec![],
        drones: vec![],
        fighters: vec![],
        implants: vec![],
        boosters: vec![],
        cargo: vec![],
        fleet: Fleet::default(),
        projected: vec![],
        environment: Environment::default(),
        damage_pattern: None,
        target_profile: None,
        overrides: vec![],
        options: Options { validate: true, ..Default::default() },
    };
    for line in lines {
        if line.starts_with("[Empty") {
            continue;
        }
        let (line, offline) = match line.strip_suffix("/OFFLINE").or_else(|| line.strip_suffix("/offline")) {
            Some(l) => (l.trim(), true),
            None => (line, false),
        };
        let (line, mref) = mut_ref(line);
        let mutation = match mref {
            Some(n) => Some(muts.get(&n).cloned().ok_or(format!("mutation [{n}] not defined"))?),
            None => None,
        };
        // "Name xN" => drone / fighter / cargo
        if let Some(pos) = line.rfind(" x") {
            if let Ok(n) = line[pos + 2..].trim().parse::<u32>() {
                let name = line[..pos].trim();
                let Some(mut tid) = ds.type_by_name(name) else { return Err(format!("unknown item '{name}'")) };
                if let Some(m) = &mutation {
                    tid = mutated_type(ds, m);
                }
                let t = &ds.types[&tid];
                match t.category {
                    18 => req.drones.push(DroneReq { type_id: tid, quantity: n, active: Some(n), mutation: mutation.clone() }),
                    87 => req.fighters.push(FighterReq { type_id: tid, quantity: Some(n), active: true, abilities: None }),
                    _ => req.cargo.push(CargoReq { type_id: tid, quantity: n }),
                }
                continue;
            }
        }
        let mut parts = line.splitn(2, ',');
        let name = parts.next().unwrap().trim();
        let charge = parts.next().map(|s| s.trim());
        let Some(mut tid) = ds.type_by_name(name) else { return Err(format!("unknown item '{name}'")) };
        if let Some(m) = &mutation {
            tid = mutated_type(ds, m);
        }
        let t = &ds.types[&tid];
        match t.category {
            20 => {
                // implants vs boosters: boosters have attribute boosterness (1087)
                if t.attr(1087).is_some() {
                    req.boosters.push(BoosterReq { type_id: tid, side_effects: vec![] })
                } else {
                    req.implants.push(tid)
                }
            }
            18 => req.drones.push(DroneReq { type_id: tid, quantity: 1, active: Some(1), mutation: mutation.clone() }),
            8 => req.cargo.push(CargoReq { type_id: tid, quantity: 1 }),
            _ => {
                if t.group == 1306 {
                    // T3D mode
                    req.ship.mode_type_id = Some(tid);
                    continue;
                }
                let slot = infer_slot(ds, t);
                let charge_type_id = match charge {
                    Some(c) => Some(ds.type_by_name(c).ok_or(format!("unknown charge '{c}'"))?),
                    None => None,
                };
                let active_capable = t.effects.iter().any(|(e, _)| ds.effects.get(e).map(|x| x.category == 1).unwrap_or(false))
                    || t.attr(6).map(|v| v != 0.0).unwrap_or(false);
                let state = if offline {
                    State::Offline
                } else if active_capable && !matches!(slot, Some(Slot::Rig) | Some(Slot::Subsystem)) {
                    State::Active
                } else {
                    State::Online
                };
                req.modules.push(ModuleReq { type_id: tid, slot, state: Some(state), charge_type_id, mutation: mutation.clone(), spool: None });
            }
        }
    }
    Ok(req)
}

/// Python `repr(float)` of Pyfa's `floatUnerr(v)` (7 significant digits kept), as Pyfa prints mutated values.
fn py_float(v: f64) -> String {
    let v = if v == 0.0 || !v.is_finite() {
        v
    } else {
        let rf = 7 - (v.abs().log10()).ceil() as i32;
        if rf >= 0 {
            format!("{:.*}", rf as usize, v).parse().unwrap_or(v)
        } else {
            let p = 10f64.powi(-rf);
            (v / p).round() * p
        }
    };
    if v.is_infinite() {
        return if v > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let a = v.abs();
    if a != 0.0 && !(1e-4..1e16).contains(&a) {
        // Python switches to exponent notation: 1e-05, 1.5e+16
        let e = format!("{v:e}");
        let (m, x) = e.split_once('e').unwrap();
        let xi: i32 = x.parse().unwrap();
        return format!("{m}e{}{:02}", if xi < 0 { '-' } else { '+' }, xi.abs());
    }
    if v.fract() == 0.0 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

/// Pyfa's drone market-group order (service/port/eft.py DRONE_ORDER), by market group id.
fn drone_order(mg: Option<u32>) -> usize {
    match mg {
        Some(837 | 1531) => 0,   // Light Scout Drones
        Some(3881) => 1,         // Light Hybrid Drones
        Some(838 | 1532) => 2,   // Medium Scout Drones
        Some(3882) => 3,         // Medium Hybrid Drones
        Some(839 | 359) => 4,    // Heavy Attack Drones
        Some(3883) => 5,         // Heavy Hybrid Drones
        Some(911 | 1533) => 6,   // Sentry Drones
        Some(843 | 1586) => 7,   // Combat Utility Drones
        Some(841 | 1029) => 8,   // Electronic Warfare Drones
        Some(842 | 1030) => 9,   // Logistic Drones
        Some(158 | 358) => 10,   // Mining Drones
        Some(1643 | 1646) => 11, // Salvage Drones
        _ => 12,
    }
}

const FIGHTER_ORDER: [&str; 6] = ["Light Fighter", "Structure Light Fighter", "Heavy Fighter", "Structure Heavy Fighter", "Support Fighter", "Structure Support Fighter"];

/// EFT export, byte-for-byte what Pyfa's `exportEft` (all options on) writes for the same fit after
/// Pyfa's GUI `fill()`: header, blank line, sections joined by two blank lines (modules by rack
/// LOW/MED/HIGH/RIG/SUBSYSTEM/SERVICE with `[Empty X slot]` fillers; drones+fighters; implants+boosters;
/// cargo; mutation details), sub-sections by one blank line, no trailing newline. Like Pyfa, the T3D
/// mode is not written (Pyfa's EFT format has no mode line; importers default it).
pub fn export(ds: &Dataset, req: &FitRequest, name: &str) -> String {
    let n = |id: u32| ds.types.get(&id).map(|t| t.name.clone()).unwrap_or_else(|| id.to_string());
    let attr = |id: u32, a: &str| -> f64 { ds.types.get(&id).and_then(|t| { let id = ds.attr_id(a); t.attrs.iter().find(|x| x.0 == id).map(|x| x.1) }).unwrap_or(0.0) };
    let group_of = |id: u32| ds.types.get(&id).and_then(|t| ds.groups.get(&t.group));
    // slot totals after modifiers (subsystems, structure rigs, ...)
    let totals: Option<crate::engine::Fit> = crate::engine::Fit::build(ds, req).ok();
    let total = |a: &str| -> i64 { totals.as_ref().map(|f| f.get(f.ship, ds.attr_id(a))).unwrap_or(0.0) as i64 };
    let mut muts: Vec<Mutation> = Vec::new();
    let mut sections: Vec<String> = Vec::new();

    // Section 1: modules
    let mut racks: Vec<String> = Vec::new();
    for (slot, label, tattr) in [
        (Slot::Low, "Low", "lowSlots"),
        (Slot::Mid, "Med", "medSlots"),
        (Slot::High, "High", "hiSlots"),
        (Slot::Rig, "Rig", "rigSlots"),
        (Slot::Subsystem, "Subsystem", "maxSubSystems"),
        (Slot::Service, "Service", "serviceSlots"),
    ] {
        let mut lines: Vec<String> = Vec::new();
        for m in req.modules.iter().filter(|m| m.slot.or_else(|| ds.types.get(&m.type_id).and_then(|t| infer_slot(ds, t))) == Some(slot)) {
            let mut l = match &m.mutation {
                Some(mu) => n(mu.base_type_id),
                None => n(m.type_id),
            };
            let off = if m.state == Some(State::Offline) { " /offline" } else { "" };
            let mtag = match &m.mutation {
                Some(mu) if mu.mutaplasmid_type_id.is_some() => {
                    muts.push(mu.clone());
                    format!(" [{}]", muts.len())
                }
                _ => String::new(),
            };
            if let Some(c) = m.charge_type_id {
                l += &format!(", {}", n(c));
            }
            l += off;
            l += &mtag;
            lines.push(l);
        }
        let free = total(tattr) - lines.len() as i64;
        for _ in 0..free.max(0) {
            lines.push(format!("[Empty {label} slot]"));
        }
        if !lines.is_empty() {
            racks.push(lines.join("\n"));
        }
    }
    if !racks.is_empty() {
        sections.push(racks.join("\n\n"));
    }

    // Section 2: drones, fighters
    let mut minion: Vec<String> = Vec::new();
    let mut drones: Vec<&DroneReq> = req.drones.iter().collect();
    let dbase = |d: &DroneReq| d.mutation.as_ref().map(|m| m.base_type_id).unwrap_or(d.type_id);
    let dmut = |d: &DroneReq| d.mutation.as_ref().map(|m| m.mutaplasmid_type_id.is_some()).unwrap_or(false);
    drones.sort_by_cached_key(|d| {
        let mg = ds.types.get(&dbase(d)).and_then(|t| t.market_group);
        let full = if dmut(d) { ds.types.get(&d.type_id).map(|t| t.name.clone()).unwrap_or_default() } else { n(d.type_id) };
        (drone_order(mg), dmut(d), full)
    });
    let mut dl: Vec<String> = Vec::new();
    for d in drones {
        let mtag = if dmut(d) {
            muts.push(d.mutation.clone().unwrap());
            format!(" [{}]", muts.len())
        } else {
            String::new()
        };
        dl.push(format!("{} x{}{}", n(dbase(d)), d.quantity, mtag));
    }
    if !dl.is_empty() {
        minion.push(dl.join("\n"));
    }
    let mut fighters: Vec<&FighterReq> = req.fighters.iter().collect();
    fighters.sort_by_cached_key(|f| {
        let g = group_of(f.type_id).map(|g| g.name.as_str()).unwrap_or("");
        (FIGHTER_ORDER.iter().position(|x| *x == g).unwrap_or(FIGHTER_ORDER.len()), n(f.type_id))
    });
    let fl: Vec<String> = fighters
        .iter()
        .map(|f| {
            let max = attr(f.type_id, "fighterSquadronMaxSize") as u32;
            let q = f.quantity.map(|q| if q >= max { max } else { q }).unwrap_or(max);
            format!("{} x{}", n(f.type_id), q)
        })
        .collect();
    if !fl.is_empty() {
        minion.push(fl.join("\n"));
    }
    if !minion.is_empty() {
        sections.push(minion.join("\n\n"));
    }

    // Section 3: implants (by implantness), boosters (by boosterness)
    let mut charsec: Vec<String> = Vec::new();
    let mut imps: Vec<u32> = req.implants.clone();
    imps.sort_by(|a, b| attr(*a, "implantness").partial_cmp(&attr(*b, "implantness")).unwrap());
    if !imps.is_empty() {
        charsec.push(imps.iter().map(|i| n(*i)).collect::<Vec<_>>().join("\n"));
    }
    let mut boos: Vec<u32> = req.boosters.iter().map(|b| b.type_id).collect();
    boos.sort_by(|a, b| attr(*a, "boosterness").partial_cmp(&attr(*b, "boosterness")).unwrap());
    if !boos.is_empty() {
        charsec.push(boos.iter().map(|i| n(*i)).collect::<Vec<_>>().join("\n"));
    }
    if !charsec.is_empty() {
        sections.push(charsec.join("\n\n"));
    }

    // Section 4: cargo by (category name, group name, type name)
    let mut cargo: Vec<&CargoReq> = req.cargo.iter().collect();
    cargo.sort_by_cached_key(|c| {
        let g = group_of(c.type_id);
        let cat = g.and_then(|g| ds.categories.get(&g.category)).cloned().unwrap_or_default();
        (cat, g.map(|g| g.name.clone()).unwrap_or_default(), n(c.type_id))
    });
    if !cargo.is_empty() {
        sections.push(cargo.iter().map(|c| format!("{} x{}", n(c.type_id), c.quantity)).collect::<Vec<_>>().join("\n"));
    }

    // Section 5: mutation details
    if !muts.is_empty() {
        let blocks: Vec<String> = muts
            .iter()
            .enumerate()
            .map(|(k, m)| {
                let mut kv: Vec<(String, f64)> = m
                    .attributes
                    .iter()
                    .map(|(a, v)| (a.parse::<u32>().ok().and_then(|id| ds.attrs.get(&id)).map(|x| x.name.clone()).unwrap_or(a.clone()), *v))
                    .collect();
                kv.sort_by(|a, b| a.0.cmp(&b.0));
                let attrs = kv.iter().map(|(a, v)| format!("{a} {}", py_float(*v))).collect::<Vec<_>>().join(", ");
                format!("[{}] {}\n  {}\n  {}", k + 1, n(m.base_type_id), n(m.mutaplasmid_type_id.unwrap()), attrs)
            })
            .collect();
        sections.push(blocks.join("\n"));
    }
    format!("[{}, {}]\n\n{}", n(req.ship.type_id), name, sections.join("\n\n\n"))
}
