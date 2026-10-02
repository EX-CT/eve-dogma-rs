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

pub fn export(ds: &Dataset, req: &FitRequest, name: &str) -> String {
    let n = |id: u32| ds.types.get(&id).map(|t| t.name.clone()).unwrap_or_else(|| id.to_string());
    let mut out = format!("[{}, {}]\n", n(req.ship.type_id), name);
    let mut muts: Vec<Mutation> = Vec::new();
    let mut tag = |m: &Option<Mutation>| -> String {
        match m {
            Some(m) => {
                muts.push(m.clone());
                format!(" [{}]", muts.len())
            }
            None => String::new(),
        }
    };
    for slot in [Slot::Low, Slot::Mid, Slot::High, Slot::Rig, Slot::Subsystem, Slot::Service] {
        let mut any = false;
        for m in req.modules.iter().filter(|m| m.slot.or_else(|| ds.types.get(&m.type_id).and_then(|t| infer_slot(ds, t))) == Some(slot)) {
            any = true;
            match &m.mutation {
                Some(mu) => out += &n(mu.base_type_id),
                None => out += &n(m.type_id),
            }
            if let Some(c) = m.charge_type_id {
                out += &format!(", {}", n(c));
            }
            if m.state == Some(State::Offline) {
                out += " /OFFLINE";
            }
            out += &tag(&m.mutation);
            out += "\n";
        }
        if any {
            out += "\n";
        }
    }
    for d in &req.drones {
        let nm = d.mutation.as_ref().map(|m| m.base_type_id).unwrap_or(d.type_id);
        out += &format!("{} x{}{}\n", n(nm), d.quantity, tag(&d.mutation));
    }
    for f in &req.fighters {
        out += &format!("{} x{}\n", n(f.type_id), f.quantity.unwrap_or(1));
    }
    if !req.implants.is_empty() || !req.boosters.is_empty() {
        out += "\n";
        for i in &req.implants {
            out += &format!("{}\n", n(*i));
        }
        for b in &req.boosters {
            out += &format!("{}\n", n(b.type_id));
        }
    }
    if !req.cargo.is_empty() {
        out += "\n";
        for c in &req.cargo {
            out += &format!("{} x{}\n", n(c.type_id), c.quantity);
        }
    }
    drop(tag);
    if !muts.is_empty() {
        out += "\n";
        for (k, m) in muts.iter().enumerate() {
            out += &format!("[{}] {}\n", k + 1, n(m.base_type_id));
            if let Some(p) = m.mutaplasmid_type_id {
                out += &format!("  {}\n", n(p));
            }
            let kv: Vec<String> = m
                .attributes
                .iter()
                .map(|(a, v)| {
                    let an = a.parse::<u32>().ok().and_then(|id| ds.attrs.get(&id)).map(|x| x.name.clone()).unwrap_or(a.clone());
                    format!("{an} {v}")
                })
                .collect();
            if !kv.is_empty() {
                out += &format!("  {}\n", kv.join(", "));
            }
        }
    }
    out
}
