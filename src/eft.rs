//! Minimal EFT text import/export (format: public EVE fitting text, written clean-room).
use crate::data::Dataset;
use crate::engine::infer_slot;
use crate::request::*;

pub fn parse(ds: &Dataset, text: &str) -> Result<FitRequest, String> {
    let mut lines = text.lines().map(|l| l.trim()).filter(|l| !l.is_empty());
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
        // "Name xN" => drone / fighter / cargo
        if let Some(pos) = line.rfind(" x") {
            if let Ok(n) = line[pos + 2..].trim().parse::<u32>() {
                let name = line[..pos].trim();
                let Some(tid) = ds.type_by_name(name) else { return Err(format!("unknown item '{name}'")) };
                let t = &ds.types[&tid];
                match t.category {
                    18 => req.drones.push(DroneReq { type_id: tid, quantity: n, active: Some(n), mutation: None }),
                    87 => req.fighters.push(FighterReq { type_id: tid, quantity: Some(n), active: true, abilities: None }),
                    _ => req.cargo.push(CargoReq { type_id: tid, quantity: n }),
                }
                continue;
            }
        }
        let mut parts = line.splitn(2, ',');
        let name = parts.next().unwrap().trim();
        let charge = parts.next().map(|s| s.trim());
        let Some(tid) = ds.type_by_name(name) else { return Err(format!("unknown item '{name}'")) };
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
            18 => req.drones.push(DroneReq { type_id: tid, quantity: 1, active: Some(1), mutation: None }),
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
                req.modules.push(ModuleReq { type_id: tid, slot, state: Some(state), charge_type_id, mutation: None, spool: None });
            }
        }
    }
    Ok(req)
}

pub fn export(ds: &Dataset, req: &FitRequest, name: &str) -> String {
    let n = |id: u32| ds.types.get(&id).map(|t| t.name.clone()).unwrap_or_else(|| id.to_string());
    let mut out = format!("[{}, {}]\n", n(req.ship.type_id), name);
    for slot in [Slot::Low, Slot::Mid, Slot::High, Slot::Rig, Slot::Subsystem, Slot::Service] {
        let mut any = false;
        for m in req.modules.iter().filter(|m| m.slot.or_else(|| ds.types.get(&m.type_id).and_then(|t| infer_slot(ds, t))) == Some(slot)) {
            any = true;
            out += &n(m.type_id);
            if let Some(c) = m.charge_type_id {
                out += &format!(", {}", n(c));
            }
            if m.state == Some(State::Offline) {
                out += " /OFFLINE";
            }
            out += "\n";
        }
        if any {
            out += "\n";
        }
    }
    for d in &req.drones {
        out += &format!("{} x{}\n", n(d.type_id), d.quantity);
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
    out
}
