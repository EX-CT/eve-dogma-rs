//! Fit statistics on top of the evaluated dogma graph (Pyfa-equivalent formulas).
use crate::capsim::{self, Drain};
use crate::engine::{Fit, Kind};
use crate::request::{FitRequest, Resists, Slot, Spool, SpoolType, State};
use serde_json::{json, Map, Value};

pub fn range_factor(optimal: f64, falloff: f64, distance: Option<f64>, restricted: bool) -> f64 {
    let Some(d) = distance else { return 1.0 };
    if falloff > 0.0 {
        if restricted && d > optimal + 3.0 * falloff {
            return 0.0;
        }
        0.5f64.powf(((d - optimal).max(0.0) / falloff).powi(2))
    } else if d <= optimal {
        1.0
    } else {
        0.0
    }
}

pub fn lock_time(scan_res: f64, sig: f64) -> Option<f64> {
    if scan_res <= 0.0 || sig <= 0.0 {
        return None;
    }
    Some((40000.0 / scan_res / sig.asinh().powi(2)).min(1800.0))
}

fn float_unerr(v: f64) -> f64 {
    (v * 1e9).round() / 1e9
}

/// Pyfa eos/utils/spoolSupport.calculateSpoolup -> (value, cycles, time)
pub fn spoolup(max: f64, step: f64, cycle_s: f64, spool: Spool) -> (f64, f64, f64) {
    if max == 0.0 || step == 0.0 {
        return (0.0, 0.0, 0.0);
    }
    let cycles = match spool.kind {
        SpoolType::SpoolScale => float_unerr(max * spool.amount / step).ceil(),
        SpoolType::CycleScale => (spool.amount * float_unerr(max / step).ceil()).round(),
        SpoolType::Time => float_unerr(spool.amount / cycle_s).floor().min(float_unerr(max / step).ceil()),
        SpoolType::Cycles => spool.amount.floor().min(float_unerr(max / step).ceil()),
    };
    let v = (cycles * step).min(max);
    (v, cycles, cycles * cycle_s)
}

#[derive(Default, Clone, Copy)]
struct Dmg {
    em: f64,
    th: f64,
    ki: f64,
    ex: f64,
}
impl Dmg {
    fn total(&self) -> f64 {
        self.em + self.th + self.ki + self.ex
    }
    fn scale(&self, k: f64) -> Dmg {
        Dmg { em: self.em * k, th: self.th * k, ki: self.ki * k, ex: self.ex * k }
    }
    fn add(&mut self, o: &Dmg) {
        self.em += o.em;
        self.th += o.th;
        self.ki += o.ki;
        self.ex += o.ex;
    }
    fn vs(&self, r: &Resists) -> f64 {
        self.em * (1.0 - r.em) + self.th * (1.0 - r.thermal) + self.ki * (1.0 - r.kinetic) + self.ex * (1.0 - r.explosive)
    }
    fn json(&self) -> Value {
        json!({"em": self.em, "thermal": self.th, "kinetic": self.ki, "explosive": self.ex, "total": self.total()})
    }
}

struct Ids {
    cpu: u32,
    power: u32,
    cpu_out: u32,
    power_out: u32,
    upgrade_cost: u32,
    upgrade_cap: u32,
    speed: u32,
    duration: u32,
    cap_need: u32,
    reload: u32,
    reactivation: u32,
    charge_rate: u32,
    dmg_mult: u32,
    dmg: [u32; 4],
}

fn ids(f: &Fit) -> Ids {
    let a = |n: &str| f.ds.attr_id(n);
    Ids {
        cpu: a("cpu"),
        power: a("power"),
        cpu_out: a("cpuOutput"),
        power_out: a("powerOutput"),
        upgrade_cost: a("upgradeCost"),
        upgrade_cap: a("upgradeCapacity"),
        speed: a("speed"),
        duration: a("duration"),
        cap_need: a("capacitorNeed"),
        reload: a("reloadTime"),
        reactivation: a("moduleReactivationDelay"),
        charge_rate: a("chargeRate"),
        dmg_mult: a("damageMultiplier"),
        dmg: [a("emDamage"), a("thermalDamage"), a("kineticDamage"), a("explosiveDamage")],
    }
}

fn round6(v: f64) -> f64 {
    if v.is_finite() { (v * 1e6).round() / 1e6 } else { v }
}

/// Recursively round floats for stable, readable output.
fn tidy(v: Value) -> Value {
    match v {
        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if n.is_f64() {
                    return json!(round6(f));
                }
            }
            Value::Number(n)
        }
        Value::Array(a) => Value::Array(a.into_iter().map(tidy).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (k, tidy(v))).collect()),
        x => x,
    }
}

impl<'a> Fit<'a> {
    fn has_effect_named(&self, i: usize, names: &[&str]) -> bool {
        self.items[i].effects.iter().any(|(e, _)| self.ds.effects.get(e).map(|x| names.contains(&x.name.as_str())).unwrap_or(false))
    }

    fn raw_cycle_ms(&self, i: usize, id: &Ids) -> f64 {
        let mut v: f64 = self.get(i, id.speed).max(self.get(i, id.duration));
        for n in [
            "durationHighisGood",
            "durationSensorDampeningBurstProjector",
            "durationTargetIlluminationBurstProjector",
            "durationECMJammerBurstProjector",
            "durationWeaponDisruptionBurstProjector",
        ] {
            let a = self.ds.attr_id(n);
            if a != 0 {
                v = v.max(self.get(i, a));
            }
        }
        v
    }

    fn num_charges(&self, i: usize) -> u32 {
        let Some(c) = self.items[i].charge else { return 0 };
        let vol = self.get(c, 161);
        let cap = self.base(i, 38);
        if vol <= 0.0 { 0 } else { float_unerr(cap / vol).floor() as u32 }
    }

    fn num_shots(&self, i: usize, id: &Ids) -> u32 {
        let Some(c) = self.items[i].charge else { return 0 };
        let n = self.num_charges(i);
        if n > 0 && self.has(i, id.charge_rate) {
            let r = self.get(i, id.charge_rate);
            return if r > 0.0 { (n as f64 / r).floor() as u32 } else { 0 };
        }
        let cgd = self.ds.attr_id("crystalsGetDamaged");
        if n > 0 && self.has(c, cgd) {
            if self.get(c, cgd) == 1.0 {
                let hp = self.get(c, 9);
                let chance = self.get(c, self.ds.attr_id("crystalVolatilityChance"));
                let dmg = self.get(c, self.ds.attr_id("crystalVolatilityDamage"));
                if dmg * chance > 0.0 {
                    return ((n as f64 * hp) / (dmg * chance)).floor() as u32;
                }
            }
            return 0;
        }
        0
    }

    /// Average cycle time in ms (Pyfa getCycleParameters(...).averageTime)
    fn avg_cycle_ms(&self, i: usize, id: &Ids, factor_reload: bool) -> f64 {
        let active = self.raw_cycle_ms(i, id);
        if active == 0.0 {
            return 0.0;
        }
        let inactive = self.get(i, id.reactivation);
        let shots = self.num_shots(i, id);
        let reload = self.get(i, id.reload);
        if !factor_reload || shots == 0 || inactive >= reload {
            return active + inactive;
        }
        let early = shots as f64 - 1.0;
        ((active + inactive) * early + (active + reload)) / shots as f64
    }

    fn module_volley(&self, i: usize, id: &Ids) -> (Dmg, &'static str) {
        let it = &self.items[i];
        let kind = if self.has_effect_named(i, &["turretFitted"]) {
            "turret"
        } else if self.has_effect_named(i, &["launcherFitted"]) {
            "missile"
        } else if self.has_effect_named(i, &["empWave"]) {
            "smartbomb"
        } else if self.has_effect_named(i, &["ChainLightning"]) {
            "vorton"
        } else {
            "other"
        };
        let src = it.charge.unwrap_or(i);
        let mut mult = if self.has(i, id.dmg_mult) { self.get(i, id.dmg_mult) } else { 1.0 };
        if kind == "missile" && it.charge.is_some() {
            // missile damage is scaled by the pilot's missileDamageMultiplier (BCS etc. modify the character)
            mult *= self.get(self.char, self.ds.attr_id("missileDamageMultiplier"));
        }
        let d = Dmg {
            em: self.get(src, id.dmg[0]) * mult,
            th: self.get(src, id.dmg[1]) * mult,
            ki: self.get(src, id.dmg[2]) * mult,
            ex: self.get(src, id.dmg[3]) * mult,
        };
        (d, kind)
    }

    pub fn compute_stats(&self, req: &FitRequest) -> Value {
        let ds = self.ds;
        let id = ids(self);
        let ship = self.ship;
        let ch = self.char;
        let a = |n: &str| ds.attr_id(n);
        let g = |i: usize, n: &str| self.get(i, ds.attr_id(n));
        let factor_reload = req.options.factor_reload;
        let modules: Vec<usize> = (0..self.items.len()).filter(|&i| self.items[i].kind == Kind::Module).collect();
        let online = |i: usize| self.items[i].state >= State::Online;
        let active = |i: usize| self.items[i].state >= State::Active;

        // ---------------- resources
        let sum = |attr: u32, f: &dyn Fn(usize) -> bool| -> f64 { modules.iter().filter(|&&i| f(i)).map(|&i| self.get(i, attr)).sum() };
        let cpu_used = sum(id.cpu, &online);
        let pg_used = sum(id.power, &online);
        let calib_used: f64 = modules.iter().filter(|&&i| self.items[i].slot == Some(Slot::Rig)).map(|&i| self.get(i, id.upgrade_cost)).sum();
        let drones: Vec<usize> = (0..self.items.len()).filter(|&i| self.items[i].kind == Kind::Drone).collect();
        let fighters: Vec<usize> = (0..self.items.len()).filter(|&i| self.items[i].kind == Kind::Fighter).collect();
        let bw_used: f64 = drones.iter().map(|&i| g(i, "droneBandwidthUsed") * self.items[i].active_count as f64).sum();
        let bay_used: f64 = drones.iter().map(|&i| self.get(i, 161) * self.items[i].quantity as f64).sum();
        let fbay_used: f64 = fighters.iter().map(|&i| self.get(i, 161) * self.items[i].quantity as f64).sum();
        let cargo_used: f64 = req.cargo.iter().map(|c| ds.types.get(&c.type_id).map(|t| t.volume).unwrap_or(0.0) * c.quantity as f64).sum();
        let count_slot = |s: Slot| modules.iter().filter(|&&i| self.items[i].slot == Some(s)).count();
        let turrets_used = modules.iter().filter(|&&i| self.has_effect_named(i, &["turretFitted"])).count();
        let launchers_used = modules.iter().filter(|&&i| self.has_effect_named(i, &["launcherFitted"])).count();
        let usage = |u: f64, t: f64| json!({"used": u, "total": t});
        let slot_tot = |n: &str| g(ship, n);
        let fighter_class = |i: usize| -> &'static str {
            if g(i, "fighterSquadronIsHeavy") > 0.0 {
                "heavy"
            } else if g(i, "fighterSquadronIsSupport") > 0.0 {
                "support"
            } else {
                "light"
            }
        };
        let tubes_used = fighters.iter().filter(|&&i| self.items[i].active_count > 0).count();
        let class_used = |c: &str| fighters.iter().filter(|&&i| self.items[i].active_count > 0 && fighter_class(i) == c).count() as f64;
        let resources = json!({
            "cpu": usage(cpu_used, self.get(ship, id.cpu_out)),
            "power": usage(pg_used, self.get(ship, id.power_out)),
            "calibration": usage(calib_used, self.get(ship, id.upgrade_cap)),
            "drone_bandwidth": usage(bw_used, g(ship, "droneBandwidth")),
            "drone_bay": usage(bay_used, g(ship, "droneCapacity")),
            "fighter_bay": usage(fbay_used, g(ship, "fighterCapacity")),
            "cargo": usage(cargo_used, self.get(ship, 38)),
            "slots": {
                "high": usage(count_slot(Slot::High) as f64, slot_tot("hiSlots")),
                "mid": usage(count_slot(Slot::Mid) as f64, slot_tot("medSlots")),
                "low": usage(count_slot(Slot::Low) as f64, slot_tot("lowSlots")),
                "rig": usage(count_slot(Slot::Rig) as f64, slot_tot("rigSlots")),
                "subsystem": usage(count_slot(Slot::Subsystem) as f64, slot_tot("maxSubSystems")),
                "service": usage(count_slot(Slot::Service) as f64, slot_tot("serviceSlots")),
            },
            "hardpoints": {
                "turret": usage(turrets_used as f64, slot_tot("turretSlotsLeft")),
                "launcher": usage(launchers_used as f64, slot_tot("launcherSlotsLeft")),
            },
            "fighter_tubes": {
                "total": usage(tubes_used as f64, g(ship, "fighterTubes")),
                "light": usage(class_used("light"), g(ship, "fighterLightSlots")),
                "support": usage(class_used("support"), g(ship, "fighterSupportSlots")),
                "heavy": usage(class_used("heavy"), g(ship, "fighterHeavySlots")),
            },
        });

        // ---------------- offense
        let tp = req.target_profile.clone().unwrap_or_default();
        let tp_res = Resists { em: tp.em, thermal: tp.thermal, kinetic: tp.kinetic, explosive: tp.explosive };
        let default_spool = req.options.default_spool.unwrap_or(Spool { kind: SpoolType::SpoolScale, amount: 1.0 });
        let mut weapons = Vec::new();
        let mut w_vol = Dmg::default();
        let mut w_dps = Dmg::default();
        for &i in &modules {
            if !active(i) {
                continue;
            }
            let (base, kind) = self.module_volley(i, &id);
            if base.total() == 0.0 {
                continue;
            }
            let cyc = self.avg_cycle_ms(i, &id, factor_reload);
            let raw = self.raw_cycle_ms(i, &id);
            let spool = self.items[i].spool.unwrap_or(default_spool);
            let (sp, _, _) = spoolup(g(i, "damageMultiplierBonusMax"), g(i, "damageMultiplierBonusPerCycle"), raw / 1000.0, spool);
            let vol_spooled = base.scale(1.0 + sp);
            let dps = if cyc > 0.0 { vol_spooled.scale(1000.0 / cyc) } else { Dmg::default() };
            w_vol.add(&vol_spooled); // Pyfa reports spooled volley
            w_dps.add(&dps);
            let opt = g(i, "maxRange");
            let fo = g(i, "falloff");
            let mut w = json!({
                "module_index": self.items[i].req_index, "type_id": self.items[i].type_id,
                "name": ds.types[&self.items[i].type_id].name, "kind": kind,
                "charge_type_id": self.items[i].charge.map(|c| self.items[c].type_id),
                "volley": vol_spooled.json(), "dps": dps.json(), "cycle_time_ms": cyc,
            });
            if kind == "turret" {
                w["optimal_m"] = json!(opt);
                w["falloff_m"] = json!(fo);
                w["tracking"] = json!(g(i, "trackingSpeed"));
            } else if kind == "missile" {
                if let Some(c) = self.items[i].charge {
                    // Pyfa missileMaxRangeData: flight time + ship radius bonus, acceleration phase,
                    // floor/ceil blend, FoF limit, centre-to-surface (eos/saveddata/module.py, LGPL)
                    let vel = g(c, "maxVelocity");
                    if vel > 0.0 {
                        let radius = g(ship, "radius");
                        let ft = g(c, "explosionDelay") / 1000.0 + radius / vel;
                        let ft = (ft * 1e9).round() / 1e9; // floatUnerr
                        let accel_cap = g(c, "mass") * g(c, "agility") / 1e6;
                        let range_at = |t: f64| {
                            let acc = t.min(accel_cap);
                            vel / 2.0 * acc + vel * (t - acc)
                        };
                        let (lt, ht) = (ft.floor(), ft.ceil());
                        let (mut lr, mut hr) = (range_at(lt), range_at(ht));
                        if self.has_effect_named(c, &["fofMissileLaunching"]) {
                            let lim = g(c, "maxFOFTargetRange");
                            if lim > 0.0 {
                                lr = lr.min(lim);
                                hr = hr.min(lim);
                            }
                        }
                        lr = (lr - radius).max(0.0);
                        hr = (hr - radius).max(0.0);
                        let hc = ft - lt;
                        w["range_m"] = json!(lr * (1.0 - hc) + hr * hc);
                    }
                    w["explosion_radius"] = json!(g(c, "aoeCloudSize"));
                    w["explosion_velocity"] = json!(g(c, "aoeVelocity"));
                }
            } else if kind == "smartbomb" {
                w["range_m"] = json!(g(i, "empFieldRange"));
            }
            if sp > 0.0 {
                w["spool_multiplier"] = json!(1.0 + sp);
                w["volley_unspooled"] = base.json();
            }
            weapons.push(w);
        }
        let mut d_vol = Dmg::default();
        let mut d_dps = Dmg::default();
        let mut drone_out = Vec::new();
        for &i in &drones {
            let n = self.items[i].active_count as f64;
            if n == 0.0 {
                continue;
            }
            let mult = if self.has(i, id.dmg_mult) { self.get(i, id.dmg_mult) } else { 1.0 };
            let v = Dmg { em: self.get(i, id.dmg[0]), th: self.get(i, id.dmg[1]), ki: self.get(i, id.dmg[2]), ex: self.get(i, id.dmg[3]) }.scale(mult * n);
            let cyc = self.raw_cycle_ms(i, &id);
            if v.total() == 0.0 || cyc == 0.0 {
                continue;
            }
            let dps = v.scale(1000.0 / cyc);
            d_vol.add(&v);
            d_dps.add(&dps);
            drone_out.push(json!({"drone_index": self.items[i].req_index, "type_id": self.items[i].type_id, "name": ds.types[&self.items[i].type_id].name, "count": n, "volley": v.json(), "dps": dps.json()}));
        }
        let mut f_vol = Dmg::default();
        let mut f_dps = Dmg::default();
        let mut fighter_out = Vec::new();
        for &i in &fighters {
            let n = self.items[i].active_count as f64;
            if n == 0.0 {
                continue;
            }
            let mut fv = Dmg::default();
            let mut fd = Dmg::default();
            for (eff, prefix) in [("fighterAbilityAttackM", "fighterAbilityAttackMissile"), ("fighterAbilityMissiles", "fighterAbilityMissiles")] {
                let eid = ds.effect_id(eff);
                let Some(&(_, def)) = self.items[i].effects.iter().find(|(e, _)| *e == eid) else { continue };
                let used = match &self.items[i].fighter_abilities {
                    Some(l) => l.contains(&eid),
                    None => def,
                };
                if !used {
                    continue;
                }
                let m = g(i, &format!("{prefix}DamageMultiplier"));
                let m = if m == 0.0 { 1.0 } else { m };
                let v = Dmg {
                    em: g(i, &format!("{prefix}DamageEM")),
                    th: g(i, &format!("{prefix}DamageTherm")),
                    ki: g(i, &format!("{prefix}DamageKin")),
                    ex: g(i, &format!("{prefix}DamageExp")),
                }
                .scale(m * n);
                let dur = g(i, &format!("{prefix}Duration"));
                fv.add(&v);
                if dur > 0.0 {
                    fd.add(&v.scale(1000.0 / dur));
                }
            }
            if fv.total() > 0.0 {
                f_vol.add(&fv);
                f_dps.add(&fd);
                fighter_out.push(json!({"fighter_index": self.items[i].req_index, "type_id": self.items[i].type_id, "name": ds.types[&self.items[i].type_id].name, "squadron_size": n, "volley": fv.json(), "dps": fd.json()}));
            }
        }
        let mut t_vol = w_vol;
        t_vol.add(&d_vol);
        t_vol.add(&f_vol);
        let mut t_dps = w_dps;
        t_dps.add(&d_dps);
        t_dps.add(&f_dps);
        let offense = json!({
            "weapons": weapons, "drones": drone_out, "fighters": fighter_out,
            "total": {"weapon_dps": w_dps.total(), "weapon_volley": w_vol.total(), "drone_dps": d_dps.total(), "drone_volley": d_vol.total(),
                      "fighter_dps": f_dps.total(), "fighter_volley": f_vol.total(), "dps": t_dps.json(), "volley": t_vol.json()},
            "vs_target_profile": {"dps": t_dps.vs(&tp_res), "volley": t_vol.vs(&tp_res)},
        });

        // ---------------- defense
        let dp = req.damage_pattern.unwrap_or(Resists { em: 25.0, thermal: 25.0, kinetic: 25.0, explosive: 25.0 });
        let dp_tot = (dp.em + dp.thermal + dp.kinetic + dp.explosive).max(1e-12);
        let layer_res = |prefix: &str| -> [f64; 4] {
            let names: [String; 4] = if prefix.is_empty() {
                ["emDamageResonance".into(), "thermalDamageResonance".into(), "kineticDamageResonance".into(), "explosiveDamageResonance".into()]
            } else {
                [format!("{prefix}EmDamageResonance"), format!("{prefix}ThermalDamageResonance"), format!("{prefix}KineticDamageResonance"), format!("{prefix}ExplosiveDamageResonance")]
            };
            [g(ship, &names[0]), g(ship, &names[1]), g(ship, &names[2]), g(ship, &names[3])]
        };
        let effectivify = |amount: f64, r: [f64; 4]| {
            let div = (dp.em * r[0] + dp.thermal * r[1] + dp.kinetic * r[2] + dp.explosive * r[3]) / dp_tot;
            if div == 0.0 { amount } else { amount / div }
        };
        let (rs, ra, rh) = (layer_res("shield"), layer_res("armor"), layer_res(""));
        let hp_s = g(ship, "shieldCapacity");
        let hp_a = g(ship, "armorHP");
        let hp_h = self.get(ship, 9);
        let (e_s, e_a, e_h) = (effectivify(hp_s, rs), effectivify(hp_a, ra), effectivify(hp_h, rh));
        let res_json = |r: [f64; 4]| json!({"em": r[0], "thermal": r[1], "kinetic": r[2], "explosive": r[3]});
        // local repairs
        let mut shield_rep = 0.0;
        let mut armor_rep = 0.0;
        let mut hull_rep = 0.0;
        for &i in &modules {
            if !active(i) {
                continue;
            }
            let dur = self.get(i, id.duration) / 1000.0;
            if dur <= 0.0 {
                continue;
            }
            if self.has_effect_named(i, &["shieldBoosting", "fueledShieldBoosting"]) {
                shield_rep += g(i, "shieldBonus") / dur;
            }
            if self.has_effect_named(i, &["armorRepair"]) {
                armor_rep += g(i, "armorDamageAmount") / dur;
            }
            if self.has_effect_named(i, &["fueledArmorRepair"]) {
                let paste = self.items[i].charge.map(|c| ds.types[&self.items[c].type_id].name == "Nanite Repair Paste").unwrap_or(false);
                armor_rep += g(i, "armorDamageAmount") * if paste { 3.0 } else { 1.0 } / dur;
            }
            if self.has_effect_named(i, &["structureRepair"]) {
                hull_rep += g(i, "structureDamageAmount") / dur;
            }
        }
        // incoming remote repairs (Pyfa __getAppliedRr diminishing-returns formula)
        {
            let mut lists: [Vec<(f64, f64)>; 3] = [Vec::new(), Vec::new(), Vec::new()];
            for ps in &self.proj_special {
                if let crate::engine::ProjSpecial::Rep { item, layer, amount, mult, factor } = *ps {
                    let dur = self.get(item, id.duration) / 1000.0;
                    if dur > 0.0 {
                        lists[layer as usize].push((self.get(item, amount) * mult * factor, dur));
                    }
                }
            }
            let applied = |l: &Vec<(f64, f64)>| -> f64 {
                let total: f64 = l.iter().map(|(a, c)| a / c.trunc()).sum();
                l.iter()
                    .map(|(a, c)| {
                        let rrps = a / c.trunc();
                        let m = 7000.0 + rrps * 20.0;
                        (1.0 - (((rrps + m) / (total + m)) - 1.0).powi(2)) * a / c
                    })
                    .sum()
            };
            shield_rep += applied(&lists[0]);
            armor_rep += applied(&lists[1]);
            hull_rep += applied(&lists[2]);
        }
        let shield_rr_s = g(ship, "shieldRechargeRate") / 1000.0;
        let passive = if shield_rr_s > 0.0 { 10.0 / shield_rr_s * 0.5 * 0.5 * hp_s } else { 0.0 };
        let mut defense = json!({
            "hp": {"shield": hp_s, "armor": hp_a, "hull": hp_h, "total": hp_s + hp_a + hp_h},
            "resonance": {"shield": res_json(rs), "armor": res_json(ra), "hull": res_json(rh)},
            "ehp": {"shield": e_s, "armor": e_a, "hull": e_h, "total": e_s + e_a + e_h},
            "damage_pattern": {"em": dp.em, "thermal": dp.thermal, "kinetic": dp.kinetic, "explosive": dp.explosive},
            "tank": {
                "raw": {"passive_shield": passive, "shield_repair": shield_rep, "armor_repair": armor_rep, "hull_repair": hull_rep},
                "effective": {"passive_shield": effectivify(passive, rs), "shield_repair": effectivify(shield_rep, rs),
                              "armor_repair": effectivify(armor_rep, ra), "hull_repair": effectivify(hull_rep, rh)},
            },
        });

        // ---------------- capacitor
        let cap = g(ship, "capacitorCapacity");
        let rr = self.get(ship, a("rechargeRate"));
        let peak = if rr > 0.0 { 10.0 / (rr / 1000.0) * 0.5 * 0.5 * cap } else { 0.0 };
        let mut drains = Vec::new();
        let mut cap_used = 0.0;
        let mut cap_added = 0.0;
        let booster_grp = |i: usize| ds.groups.get(&self.items[i].group).map(|g| g.name == "Capacitor Booster").unwrap_or(false);
        let mut module_rows = Vec::new();
        for &i in &modules {
            let mut cap_need = self.get(i, id.cap_need);
            let is_inj = booster_grp(i);
            if is_inj {
                cap_need = -self.items[i].charge.map(|c| g(c, "capacitorBonus")).unwrap_or(0.0);
            }
            if self.has_effect_named(i, &["energyNosferatuFalloff"]) && !req.options.nos_no_target_cap {
                // local nosferatu counts as cap income (assumes the target has cap), like Pyfa
                cap_need = -g(i, "powerTransferAmount");
            }
            let cyc_raw = self.raw_cycle_ms(i, &id);
            let full = cyc_raw + self.get(i, id.reactivation);
            let mut row = json!({"module_index": self.items[i].req_index, "type_id": self.items[i].type_id,
                "name": ds.types[&self.items[i].type_id].name, "slot": self.items[i].slot, "state": self.items[i].state,
                "cpu": self.get(i, id.cpu), "power": self.get(i, id.power)});
            if cyc_raw > 0.0 {
                row["cycle_time_ms"] = json!(cyc_raw);
            }
            if active(i) && cap_need != 0.0 && full > 0.0 {
                // Pyfa forces reload into capacitor boosters' average cycle (module.forceReload)
                let avg = self.avg_cycle_ms(i, &id, factor_reload || is_inj);
                let use_ = if avg > 0.0 { cap_need / (avg / 1000.0) } else { 0.0 };
                if use_ > 0.0 { cap_used += use_ } else { cap_added -= use_ }
                row["cap_use_gj_s"] = json!(use_);
                drains.push(Drain {
                    duration: full.trunc(),
                    cap_need,
                    clip_size: self.num_shots(i, &id),
                    reload_ms: self.get(i, id.reload),
                    is_injector: is_inj,
                    disable_stagger: self.has_effect_named(i, &["turretFitted"]),
                });
            }
            module_rows.push(row);
        }
        // incoming neuts / nos / cap transfers (Pyfa fit.addDrain): no stagger, after the fit's own modules
        let sig_now = g(ship, "signatureRadius");
        for ps in &self.proj_special {
            if let crate::engine::ProjSpecial::Drain { item, amount, duration, factor, resist, sign } = *ps {
                let mut need = self.get(item, amount) * factor * sign;
                if resist != 0 {
                    need *= self.get(ship, resist);
                }
                let sres = g(item, "energyNeutralizerSignatureResolution");
                if sres != 0.0 {
                    need *= (sig_now / sres).min(1.0);
                }
                let dur = self.get(item, duration);
                if need != 0.0 && dur > 0.0 {
                    if need > 0.0 { cap_used += need / (dur.trunc() / 1000.0) } else { cap_added -= need / (dur.trunc() / 1000.0) }
                    drains.push(Drain { duration: dur.trunc(), cap_need: need, clip_size: 0, reload_ms: 0.0, is_injector: false, disable_stagger: false });
                }
            }
        }
        let mut capj = json!({"capacity": cap, "recharge_time_s": rr / 1000.0, "peak_recharge_gj_s": peak,
            "use_gj_s": cap_used, "injected_gj_s": cap_added, "delta_gj_s": peak + cap_added - cap_used});
        if drains.is_empty() {
            capj["stable"] = json!(true);
            capj["stable_percent"] = json!(100.0);
        } else {
            let o = &req.options.cap_sim;
            let r = capsim::simulate(cap, rr, &drains, 1.0, o.reload || factor_reload, true, o.max_time_s.unwrap_or(6.0 * 3600.0) * 1000.0);
            let st = (r.stable_low + r.stable_high) / 2.0;
            capj["stable"] = json!(r.stable && st > 0.0);
            if r.stable && st > 0.0 {
                capj["stable_percent"] = json!((st * 100.0).min(100.0));
            } else {
                capj["depletes_in_s"] = json!(r.t_s);
            }
            capj["eve_stable_percent"] = json!(r.eve_stable * 100.0);
            capj["sim_iterations"] = json!(r.iterations);
        }

        // ---------------- sustainable tank (Pyfa Fit.sustainableTank, eos LGPL): when the capacitor is not
        // stable (or reload is factored), local cap-using repairers only run as far as peak recharge allows.
        {
            let stable_now = capj["stable"].as_bool().unwrap_or(true);
            let mut sus = [shield_rep, armor_rep, hull_rep];
            if !stable_now || factor_reload {
                let grp_of = |i: usize| ds.groups.get(&self.items[i].group).map(|g| g.name.as_str()).unwrap_or("");
                let spec = |gname: &str| -> Option<(usize, &'static str)> {
                    match gname {
                        "Shield Booster" | "Ancillary Shield Booster" => Some((0, "shieldBonus")),
                        "Armor Repair Unit" | "Ancillary Armor Repairer" => Some((1, "armorDamageAmount")),
                        "Hull Repair Unit" => Some((2, "structureDamageAmount")),
                        _ => None,
                    }
                };
                let mut adj = [0.0f64; 3];
                let mut used = cap_used;
                let mut reps: Vec<(usize, usize, &'static str, f64)> = Vec::new();
                for layer in 0..3 {
                    for &i in &modules {
                        if !active(i) {
                            continue;
                        }
                        let gname = grp_of(i);
                        let Some((l, attr)) = spec(gname) else { continue };
                        if l != layer {
                            continue;
                        }
                        let cap_need = self.get(i, id.cap_need);
                        let avg = self.avg_cycle_ms(i, &id, factor_reload);
                        let cap_use = if cap_need != 0.0 && avg > 0.0 { cap_need / (avg / 1000.0) } else { 0.0 };
                        let cyc = self.raw_cycle_ms(i, &id);
                        if cyc <= 0.0 {
                            continue;
                        }
                        let amount = g(i, attr);
                        let charge = self.items[i].charge;
                        let paste = charge.map(|c| ds.types[&self.items[c].type_id].name == "Nanite Repair Paste").unwrap_or(false);
                        if cap_use != 0.0 {
                            used -= cap_use;
                            let mult = if paste { let m = g(i, "chargedArmorDamageMultiplier"); if m == 0.0 { 1.0 } else { m } } else { 1.0 };
                            adj[l] -= amount * mult / (cyc / 1000.0);
                            reps.push((i, l, attr, cap_use));
                        } else if gname == "Ancillary Shield Booster" {
                            let reload = if factor_reload && charge.is_some() { self.get(i, id.reload) } else { 0.0 };
                            let shots = self.num_shots(i, &id).max(1) as f64;
                            let off = reload / (shots * cyc + reload);
                            adj[l] -= amount * off / (cyc / 1000.0);
                        }
                    }
                }
                let eff = |i: usize, attr: &str| {
                    let m = g(i, "chargedArmorDamageMultiplier");
                    g(i, attr) * if m == 0.0 { 1.0 } else { m } / self.get(i, id.cap_need)
                };
                reps.sort_by(|a, b| eff(b.0, b.2).partial_cmp(&eff(a.0, a.2)).unwrap_or(std::cmp::Ordering::Equal));
                let total_peak = peak + cap_added;
                for (i, l, attr, cap_use) in reps {
                    if used > total_peak {
                        break;
                    }
                    let charge = self.items[i].charge;
                    let reload = if factor_reload && charge.is_some() { self.get(i, id.reload) } else { 0.0 };
                    let cyc = self.raw_cycle_ms(i, &id);
                    let sustain = ((total_peak - used) / cap_use).min(1.0);
                    let amount = g(i, attr);
                    if charge.is_none() {
                        adj[l] += sustain * amount / (cyc / 1000.0);
                    } else {
                        let paste = ds.types[&self.items[charge.unwrap()].type_id].name == "Nanite Repair Paste";
                        let mult = if paste { let m = g(i, "chargedArmorDamageMultiplier"); if m == 0.0 { 1.0 } else { m } } else { 1.0 };
                        let shots = self.num_shots(i, &id).max(1) as f64;
                        let on = shots * cyc / (shots * cyc + reload);
                        adj[l] += sustain * amount * on * mult / (cyc / 1000.0);
                    }
                    used += cap_use;
                }
                for l in 0..3 {
                    sus[l] += adj[l];
                }
            }
            defense["tank"]["sustained"] = json!({"passive_shield": passive, "shield_repair": sus[0], "armor_repair": sus[1], "hull_repair": sus[2]});
            defense["tank"]["sustained_effective"] = json!({"passive_shield": effectivify(passive, rs), "shield_repair": effectivify(sus[0], rs),
                "armor_repair": effectivify(sus[1], ra), "hull_repair": effectivify(sus[2], rh)});
        }

        // ---------------- navigation
        let maxv = g(ship, "maxVelocity");
        let limit = g(ship, "speedLimit");
        let max_speed = if limit > 0.0 && maxv > limit { limit } else { maxv };
        let mass = self.get(ship, 4);
        let agility = g(ship, "agility");
        let base_warp = { let v = g(ship, "baseWarpSpeed"); if v == 0.0 { 1.0 } else { v } };
        let warp_mult = { let v = g(ship, "warpSpeedMultiplier"); if v == 0.0 { 1.0 } else { v } };
        let warp_need = g(ship, "warpCapacitorNeed");
        let sig = g(ship, "signatureRadius");
        let navigation = json!({
            "max_velocity": max_speed, "align_time_s": -(0.25f64.ln()) * agility * mass / 1e6, "mass": mass, "agility": agility,
            "signature_radius": sig, "warp_speed_au_s": base_warp * warp_mult,
            "max_warp_distance_au": if warp_need > 0.0 && mass > 0.0 { cap / (mass * warp_need) } else { 0.0 },
            "warp_scramble_status": g(ship, "warpScrambleStatus"),
        });

        // ---------------- targeting
        let strengths = [("radar", "scanRadarStrength"), ("ladar", "scanLadarStrength"), ("magnetometric", "scanMagnetometricStrength"), ("gravimetric", "scanGravimetricStrength")];
        let mut best = ("none", 0.0f64);
        for (n, at) in strengths {
            let v = g(ship, at);
            if v > best.1 {
                best = (n, v);
            }
        }
        let scan_res = g(ship, "scanResolution");
        let lt = |s: f64| lock_time(scan_res, s);
        let ship_targets = g(ship, "maxLockedTargets");
        let char_targets = self.get(ch, a("maxLockedTargets"));
        let targeting = json!({
            "max_targets": ship_targets.min(char_targets.max(0.0)),
            "max_range_m": g(ship, "maxTargetRange"), "scan_resolution": scan_res,
            "sensor_strength": best.1, "sensor_type": best.0,
            "probe_size": if best.1 > 0.0 { Some((sig / best.1).max(1.08)) } else { None },
            "lock_time_s": {"sig_25m": lt(25.0), "sig_40m": lt(40.0), "sig_125m": lt(125.0), "sig_400m": lt(400.0), "sig_target_profile": tp.signature_radius.and_then(lt)},
        });

        let drones_j = json!({
            "active": drones.iter().map(|&i| self.items[i].active_count).sum::<u32>(),
            "max_active": self.get(ch, a("maxActiveDrones")),
            "control_range_m": self.get(ch, a("droneControlDistance")),
        });

        let mut out = Map::new();
        out.insert("meta".into(), json!({"schema_version": 1, "engine": concat!("eve-dogma-rs ", env!("CARGO_PKG_VERSION")),
            "sde_build": ds.build, "dataset_sha256": ds.sha256}));
        let st = &ds.types[&self.items[ship].type_id];
        out.insert("ship".into(), json!({"type_id": st.id, "name": st.name, "group": ds.groups.get(&st.group).map(|g| g.name.clone())}));
        out.insert("resources".into(), resources);
        out.insert("offense".into(), offense);
        out.insert("defense".into(), defense);
        out.insert("capacitor".into(), capj);
        out.insert("navigation".into(), navigation);
        out.insert("targeting".into(), targeting);
        out.insert("drones".into(), drones_j);
        out.insert("modules".into(), Value::Array(module_rows));
        if req.options.validate {
            out.insert("violations".into(), Value::Array(self.validate(req, cpu_used, pg_used, calib_used, bw_used)));
        }
        if !self.warnings.is_empty() {
            out.insert("warnings".into(), json!(self.warnings));
        }
        match req.options.include_attributes.as_deref() {
            Some("ship") => {
                out.insert("attributes".into(), json!({"ship": self.dump_attrs(ship)}));
            }
            Some("all") => {
                let mut m = Map::new();
                m.insert("ship".into(), self.dump_attrs(ship));
                m.insert("character".into(), self.dump_attrs(ch));
                let mods: Vec<Value> = modules.iter().map(|&i| {
                    json!({"module_index": self.items[i].req_index, "type_id": self.items[i].type_id, "attributes": self.dump_attrs(i),
                           "charge": self.items[i].charge.map(|c| self.dump_attrs(c))})
                }).collect();
                m.insert("modules".into(), Value::Array(mods));
                let dr: Vec<Value> = drones.iter().map(|&i| json!({"drone_index": self.items[i].req_index, "attributes": self.dump_attrs(i)})).collect();
                m.insert("drones".into(), Value::Array(dr));
                out.insert("attributes".into(), Value::Object(m));
            }
            _ => {}
        }
        tidy(Value::Object(out))
    }

    pub fn dump_attrs(&self, i: usize) -> Value {
        let mut keys: Vec<u32> = self.items[i].attrs.keys().copied().collect();
        keys.sort();
        let mut m = Map::new();
        for k in keys {
            let name = self.ds.attrs.get(&k).map(|a| a.name.clone()).unwrap_or_else(|| k.to_string());
            m.insert(name, json!(self.get(i, k)));
        }
        Value::Object(m)
    }

    fn validate(&self, req: &FitRequest, cpu: f64, pg: f64, calib: f64, bw: f64) -> Vec<Value> {
        let ds = self.ds;
        let ship = self.ship;
        let g = |i: usize, n: &str| self.get(i, ds.attr_id(n));
        let mut v = Vec::new();
        let mut push = |code: &str, msg: String, idx: Option<usize>| v.push(json!({"code": code, "message": msg, "module_index": idx}));
        if cpu > g(ship, "cpuOutput") + 1e-9 {
            push("CPU_OVERLOAD", format!("CPU used {cpu:.2} > output {:.2}", g(ship, "cpuOutput")), None);
        }
        if pg > g(ship, "powerOutput") + 1e-9 {
            push("POWER_OVERLOAD", format!("Powergrid used {pg:.2} > output {:.2}", g(ship, "powerOutput")), None);
        }
        if calib > g(ship, "upgradeCapacity") + 1e-9 {
            push("CALIBRATION_OVERLOAD", format!("Calibration used {calib} > {}", g(ship, "upgradeCapacity")), None);
        }
        if bw > g(ship, "droneBandwidth") + 1e-9 {
            push("DRONE_BANDWIDTH", format!("Drone bandwidth used {bw} > {}", g(ship, "droneBandwidth")), None);
        }
        let modules: Vec<usize> = (0..self.items.len()).filter(|&i| self.items[i].kind == Kind::Module).collect();
        for (slot, attr) in [(Slot::High, "hiSlots"), (Slot::Mid, "medSlots"), (Slot::Low, "lowSlots"), (Slot::Rig, "rigSlots"), (Slot::Subsystem, "maxSubSystems"), (Slot::Service, "serviceSlots")] {
            let used = modules.iter().filter(|&&i| self.items[i].slot == Some(slot)).count() as f64;
            if used > g(ship, attr) {
                push("SLOTS_EXCEEDED", format!("{slot:?} slots used {used} > {}", g(ship, attr)), None);
            }
        }
        let t = modules.iter().filter(|&&i| self.has_effect_named(i, &["turretFitted"])).count() as f64;
        if t > g(ship, "turretSlotsLeft") {
            push("TURRET_HARDPOINTS", format!("turrets {t} > hardpoints {}", g(ship, "turretSlotsLeft")), None);
        }
        let l = modules.iter().filter(|&&i| self.has_effect_named(i, &["launcherFitted"])).count() as f64;
        if l > g(ship, "launcherSlotsLeft") {
            push("LAUNCHER_HARDPOINTS", format!("launchers {l} > hardpoints {}", g(ship, "launcherSlotsLeft")), None);
        }
        let ship_t = &ds.types[&self.items[ship].type_id];
        let groups_attrs: Vec<u32> = (1..=20).map(|k| ds.attr_id(&format!("canFitShipGroup{k:02}"))).filter(|x| *x != 0).collect();
        let types_attrs: Vec<u32> = (1..=11).map(|k| ds.attr_id(&format!("canFitShipType{k}"))).filter(|x| *x != 0).collect();
        let mut fitted_group: rustc_hash::FxHashMap<u32, u32> = Default::default();
        let mut fitted_type: rustc_hash::FxHashMap<u32, u32> = Default::default();
        let mut active_group: rustc_hash::FxHashMap<u32, u32> = Default::default();
        let mut online_group: rustc_hash::FxHashMap<u32, u32> = Default::default();
        for &i in &modules {
            let it = &self.items[i];
            let idx = it.req_index;
            let name = &ds.types[&it.type_id].name;
            let mt = &ds.types[&it.type_id];
            if it.slot.is_none() {
                push("NOT_FITTABLE", format!("{name} is not a fittable module"), idx);
            }
            let gr: Vec<u32> = groups_attrs.iter().filter_map(|a| mt.attr(*a)).map(|v| v as u32).filter(|v| *v != 0).collect();
            let ty: Vec<u32> = types_attrs.iter().filter_map(|a| mt.attr(*a)).map(|v| v as u32).filter(|v| *v != 0).collect();
            if (!gr.is_empty() || !ty.is_empty()) && !gr.contains(&ship_t.group) && !ty.contains(&ship_t.id) {
                push("SHIP_RESTRICTION", format!("{name} cannot be fitted to {}", ship_t.name), idx);
            }
            if it.slot == Some(Slot::Rig) {
                let rs = mt.attr(ds.attr_id("rigSize")).unwrap_or(0.0);
                let srs = g(ship, "rigSize");
                if rs != 0.0 && rs != srs {
                    push("RIG_SIZE", format!("{name} rig size {rs} != ship rig size {srs}"), idx);
                }
            }
            *fitted_group.entry(it.group).or_default() += 1;
            *fitted_type.entry(it.type_id).or_default() += 1;
            if it.state >= State::Online {
                *online_group.entry(it.group).or_default() += 1;
            }
            if it.state >= State::Active {
                *active_group.entry(it.group).or_default() += 1;
            }
            let check = |attr: &str, map: &rustc_hash::FxHashMap<u32, u32>, key: u32| -> Option<(f64, u32)> {
                let a = ds.attr_id(attr);
                let lim = mt.attr(a)?;
                let n = *map.get(&key).unwrap_or(&0);
                if lim > 0.0 && n as f64 > lim { Some((lim, n)) } else { None }
            };
            if let Some((lim, n)) = check("maxGroupFitted", &fitted_group, it.group) {
                push("MAX_GROUP_FITTED", format!("{name}: {n} fitted of group, max {lim}"), idx);
            }
            if let Some((lim, n)) = check("maxTypeFitted", &fitted_type, it.type_id) {
                push("MAX_TYPE_FITTED", format!("{name}: {n} fitted, max {lim}"), idx);
            }
            if let Some((lim, n)) = check("maxGroupOnline", &online_group, it.group) {
                push("MAX_GROUP_ONLINE", format!("{name}: {n} online of group, max {lim}"), idx);
            }
            if let Some((lim, n)) = check("maxGroupActive", &active_group, it.group) {
                push("MAX_GROUP_ACTIVE", format!("{name}: {n} active of group, max {lim}"), idx);
            }
            if let Some(c) = it.charge {
                let ct = &ds.types[&self.items[c].type_id];
                let cg: Vec<u32> = (1..=5).filter_map(|k| mt.attr(ds.attr_id(&format!("chargeGroup{k}")))).map(|v| v as u32).filter(|v| *v != 0).collect();
                if !cg.contains(&ct.group) {
                    push("CHARGE_GROUP", format!("{} cannot be loaded into {name}", ct.name), idx);
                }
                let ms = mt.attr(ds.attr_id("chargeSize"));
                let cs = ct.attr(ds.attr_id("chargeSize"));
                if let (Some(a), Some(b)) = (ms, cs) {
                    if a != b {
                        push("CHARGE_SIZE", format!("{} size {b} != launcher size {a}", ct.name), idx);
                    }
                }
                if ct.volume > mt.capacity && mt.capacity > 0.0 {
                    push("CHARGE_CAPACITY", format!("{} does not fit into {name}", ct.name), idx);
                }
            }
        }
        // skills
        let mut have: rustc_hash::FxHashMap<u32, f64> = Default::default();
        for it in self.items.iter().filter(|i| i.kind == Kind::Skill) {
            have.insert(it.type_id, it.attrs.get(&crate::engine::ATTR_SKILL_LEVEL).map(|a| a.base).unwrap_or(0.0));
        }
        let lvl_attrs = ["requiredSkill1Level", "requiredSkill2Level", "requiredSkill3Level", "requiredSkill4Level", "requiredSkill5Level", "requiredSkill6Level"];
        let skill_attrs = ["requiredSkill1", "requiredSkill2", "requiredSkill3", "requiredSkill4", "requiredSkill5", "requiredSkill6"];
        let mut missing: Vec<(u32, f64, u32)> = Vec::new();
        for it in &self.items {
            if !matches!(it.kind, Kind::Ship | Kind::Module | Kind::Charge | Kind::Drone | Kind::Fighter | Kind::Implant | Kind::Booster) {
                continue;
            }
            let t = &ds.types[&it.type_id];
            for (sa, la) in skill_attrs.iter().zip(lvl_attrs.iter()) {
                let s = t.attr(ds.attr_id(sa)).unwrap_or(0.0) as u32;
                if s == 0 {
                    continue;
                }
                let need = t.attr(ds.attr_id(la)).unwrap_or(1.0);
                if *have.get(&s).unwrap_or(&0.0) < need && !missing.iter().any(|m| m.0 == s && m.1 >= need) {
                    missing.push((s, need, it.type_id));
                }
            }
        }
        for (s, need, by) in missing {
            push("MISSING_SKILL", format!("{} {} required by {}", ds.types.get(&s).map(|t| t.name.as_str()).unwrap_or("?"), need, ds.types[&by].name), None);
        }
        let _ = req;
        v
    }
}
