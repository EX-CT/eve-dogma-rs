#!/usr/bin/env python3
"""Pyfa (eos) black-box oracle: runs Pyfa's engine headless on an EXCT FitRequest JSON and prints
comparable stats + timing. This script *uses* Pyfa as a library and is therefore GPL-3.0-or-later
(see ./LICENSE-GPL-NOTE). It is a test tool only; nothing here is linked into eve-dogma-rs.

usage: PYFA=/path/to/Pyfa python pyfa_oracle.py request.json [request2.json ...]
Requires Pyfa's eve.db (python db_update.py) and a stub `wx` module on PYTHONPATH.
"""
import json, math, os, sys, time, tempfile

PYFA = os.environ.get("PYFA", "/workspace/exct-eve/ref/pyfa")
sys.path.insert(0, PYFA)
import config  # noqa: E402
config.defPaths(tempfile.mkdtemp(prefix="pyfa-oracle-"))
import eos.config  # noqa: E402
eos.config.gamedata_connectionstring = "sqlite:///" + os.path.join(PYFA, "eve.db") + "?check_same_thread=False"
import eos.db  # noqa: E402
eos.db.saveddata_meta.create_all(eos.db.saveddata_engine)
from eos.saveddata.character import Character  # noqa: E402
from eos.saveddata.fit import Fit  # noqa: E402
from eos.saveddata.ship import Ship  # noqa: E402
from eos.saveddata.citadel import Citadel  # noqa: E402
from eos.saveddata.module import Module  # noqa: E402
from eos.saveddata.drone import Drone  # noqa: E402
from eos.saveddata.fighter import Fighter  # noqa: E402
from eos.saveddata.implant import Implant  # noqa: E402
from eos.saveddata.booster import Booster  # noqa: E402
from eos.saveddata.damagePattern import DamagePattern  # noqa: E402
from eos.const import FittingModuleState, FittingSlot, SpoolType  # noqa: E402
from eos.utils.spoolSupport import SpoolOptions  # noqa: E402

# what Pyfa's GUI passes (globalDefaultSpoolupPercentage = 100 %); matches EXCT's default spool = max
SPOOL = SpoolOptions(SpoolType.SPOOL_SCALE, eos.config.settings["globalDefaultSpoolupPercentage"], False)

STATES = {"offline": FittingModuleState.OFFLINE, "online": FittingModuleState.ONLINE,
          "active": FittingModuleState.ACTIVE, "overheated": FittingModuleState.OVERHEATED}

_chars = {}


def character(req):
    sk = req.get("character", {}).get("skills", {})
    lvl = sk.get("default_level", 0) or 0
    key = (lvl, json.dumps(sk.get("levels", {}), sort_keys=True))
    if key in _chars:
        return _chars[key]
    ch = Character("oracle-%d" % lvl, lvl)
    for k, v in sk.get("levels", {}).items():
        s = ch.getSkill(int(k))
        s.setLevel(v, ignoreRestrict=True)
    _chars[key] = ch
    return ch


def item(tid):
    if isinstance(tid, dict):
        tid = tid["type_id"]
    it = eos.db.getItem(int(tid))
    if it is None:
        raise KeyError("type %s not in Pyfa eve.db" % tid)
    return it


def mutated(cls, spec):
    mu = spec.get("mutation")
    if not mu:
        return cls(item(spec["type_id"]))
    dyn = eos.db.getDynamicItem(mu["mutaplasmid_type_id"])
    if dyn is None:
        raise KeyError("mutaplasmid %s not in Pyfa eve.db" % mu["mutaplasmid_type_id"])
    obj = cls(dyn.resultingItem, item(mu["base_type_id"]), dyn)
    vals = {int(k): v for k, v in mu.get("attributes", {}).items()}
    for aid, m in obj.mutators.items():
        if aid in vals:
            m.value = vals[aid]
    return obj


def build(req):
    sh = item(req["ship"]["type_id"])
    ship = Citadel(sh) if sh.category.name == "Structure" else Ship(sh)
    fit = Fit(ship, "oracle")
    fit.character = character(req)
    if req["ship"].get("mode_type_id"):
        fit.mode = ship.validateModeItem(eos.db.getItem(req["ship"]["mode_type_id"]))
    for m in req.get("modules", []):
        mod = mutated(Module, m)
        if m.get("charge_type_id"):
            mod.charge = item(m["charge_type_id"])
        fit.modules.append(mod)
        mod.owner = fit
        st = STATES[m.get("state", "online")]
        mod.state = st if mod.isValidState(st) else FittingModuleState.ONLINE
    for d in req.get("drones", []):
        dr = mutated(Drone, d)
        dr.amount = d.get("quantity", 1)
        dr.amountActive = d.get("active", 0) or 0
        fit.drones.append(dr)
        dr.owner = fit
    for f in req.get("fighters", []):
        fi = Fighter(item(f["type_id"]))
        if f.get("quantity"):
            fi.amount = f["quantity"]
        fi.active = bool(f.get("active", True))
        fit.fighters.append(fi)
        fi.owner = fit
    for i in req.get("implants", []):
        fit.implants.append(Implant(item(i)))
    for b in req.get("boosters", []):
        fit.boosters.append(Booster(item(b["type_id"])))
    dp = req.get("damage_pattern") or {"em": 25, "thermal": 25, "kinetic": 25, "explosive": 25}
    fit.damagePattern = DamagePattern(dp["em"], dp["thermal"], dp["kinetic"], dp["explosive"])
    fit.factorReload = bool(req.get("options", {}).get("factor_reload", False))
    return fit


def stats(fit):
    s = fit.ship
    g = s.getModifiedItemAttr
    dps = fit.getTotalDps(spoolOptions=SPOOL)
    vol = fit.getTotalVolley(spoolOptions=SPOOL)
    out = {
        "cpu_used": fit.cpuUsed, "cpu_total": g("cpuOutput"), "power_used": fit.pgUsed, "power_total": g("powerOutput"),
        "calibration_used": fit.calibrationUsed, "drone_bandwidth_used": fit.droneBandwidthUsed,
        "hp": fit.hp, "ehp": fit.ehp,
        "resonance": {l: {t: g(("%s%sDamageResonance" % (l, t.capitalize())) if l != "hull" else "%sDamageResonance" % t)
                          for t in ("em", "thermal", "kinetic", "explosive")} for l in ("shield", "armor", "hull")},
        "tank": fit.tank,
        "weapon_dps": fit.getWeaponDps(spoolOptions=SPOOL).total, "weapon_volley": fit.getWeaponVolley(spoolOptions=SPOOL).total,
        "drone_dps": fit.getDroneDps().total, "drone_volley": fit.getDroneVolley().total,
        "dps": dps.total, "volley": vol.total,
        "cap_capacity": g("capacitorCapacity"), "cap_recharge_s": g("rechargeRate") / 1000,
        "cap_stable": fit.capStable, "cap_state": fit.capState, "cap_used": fit.capUsed, "cap_recharge_peak": fit.capRecharge,
        "max_velocity": fit.maxSpeed, "align_time_s": fit.alignTime, "mass": g("mass"), "agility": g("agility"),
        "signature_radius": g("signatureRadius"), "warp_speed": fit.warpSpeed, "max_warp_distance": fit.maxWarpDistance,
        "max_targets": fit.maxTargets, "max_target_range": fit.maxTargetRange, "scan_resolution": g("scanResolution"),
        "scan_strength": fit.scanStrength, "probe_size": fit.probeSize,
        "hi_slots": g("hiSlots"), "med_slots": g("medSlots"), "low_slots": g("lowSlots"),
        "turret_hardpoints": g("turretSlotsLeft"), "launcher_hardpoints": g("launcherSlotsLeft"),
    }
    return out


def main():
    for path in sys.argv[1:]:
        req = json.load(open(path))
        try:
            fit = build(req)
        except Exception as e:  # e.g. type missing from Pyfa's (older) eve.db
            print(json.dumps({"file": os.path.basename(path), "error": repr(e)}))
            continue
        t0 = time.perf_counter()
        fit.calculateModifiedAttributes()
        st = stats(fit)
        first = time.perf_counter() - t0
        n = int(os.environ.get("ORACLE_REPEAT", "5"))
        t1 = time.perf_counter()
        for _ in range(n):
            f2 = build(req)
            f2.calculateModifiedAttributes()
            stats(f2)
        rep = (time.perf_counter() - t1) / max(n, 1)
        print(json.dumps({"file": os.path.basename(path), "stats": st, "timing_ms": {"first": first * 1000, "warm_avg_incl_build": rep * 1000}}, default=str))


if __name__ == "__main__":
    main()
