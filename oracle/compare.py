#!/usr/bin/env python3
"""Compare eve-dogma-rs against the Pyfa oracle on a corpus of EFT fits (All-V character, reload off).
usage: python3 oracle/compare.py tests/fits/*.eft  -> writes oracle/results/*.json and prints a table.
Runs the oracle in Pyfa's venv (PYFA_VENV); this file itself does not import Pyfa (MIT)."""
import json, os, subprocess, sys, math, pathlib
ROOT = pathlib.Path(__file__).resolve().parent.parent
BIN = str(ROOT / "target/release/eve-dogma")
REF = os.environ.get("EXCT_REF", "/workspace/exct-eve/ref")
TMP = pathlib.Path(os.environ.get("CMP_TMP", "/tmp/cmp")); TMP.mkdir(exist_ok=True)

def ours(st):
    r, d, o, c, n, t = st["resources"], st["defense"], st["offense"]["total"], st["capacitor"], st["navigation"], st["targeting"]
    return {
        "cpu_used": r["cpu"]["used"], "cpu_total": r["cpu"]["total"], "power_used": r["power"]["used"], "power_total": r["power"]["total"],
        "calibration_used": r["calibration"]["used"], "drone_bandwidth_used": r["drone_bandwidth"]["used"],
        "hp.shield": d["hp"]["shield"], "hp.armor": d["hp"]["armor"], "hp.hull": d["hp"]["hull"],
        "ehp.shield": d["ehp"]["shield"], "ehp.armor": d["ehp"]["armor"], "ehp.hull": d["ehp"]["hull"],
        **{f"res.{l}.{k}": d["resonance"][l][k] for l in ("shield", "armor", "hull") for k in ("em", "thermal", "kinetic", "explosive")},
        "weapon_dps": o["weapon_dps"], "weapon_volley": o["weapon_volley"], "drone_dps": o["drone_dps"] + o.get("fighter_dps", 0), "drone_volley": o["drone_volley"] + o.get("fighter_volley", 0),
        "cap_capacity": c["capacity"], "cap_recharge_s": c["recharge_time_s"], "cap_stable": c["stable"],
        "cap_state": c["stable_percent"] if c["stable"] else c.get("lasts_s"),
        "max_velocity": n["max_velocity"], "align_time_s": n["align_time_s"], "mass": n["mass"], "signature_radius": n["signature_radius"],
        "warp_speed": n["warp_speed_au_s"], "max_targets": t["max_targets"], "max_target_range": t["max_range_m"],
        "scan_resolution": t["scan_resolution"], "scan_strength": t["sensor_strength"],
        "jam_chance": t.get("jam_chance_percent"), "warp_scramble_status": n.get("warp_scramble_status"),
        "tank.armor": d["tank"]["raw"]["armor_repair"], "tank.shield": d["tank"]["raw"]["shield_repair"],
        "tank.hull": d["tank"]["raw"]["hull_repair"], "tank.passive": d["tank"]["raw"]["passive_shield"],
        **{f"stank.{k}": d["tank"]["sustained"][v] for k, v in (("armor", "armor_repair"), ("shield", "shield_repair"), ("hull", "hull_repair"))},
        "hi_slots": r["slots"]["high"]["total"], "med_slots": r["slots"]["mid"]["total"], "low_slots": r["slots"]["low"]["total"],
        **{f"w{w['module_index']}.{k}": w.get(k) for w in st["offense"]["weapons"] for k in WFIELDS},
        **{f"d{w['drone_index']}.{k}": w.get(k) for w in st["offense"]["drones"] for k in DFIELDS},
        **{f"f{w['fighter_index']}.{k}": w.get(k) for w in st["offense"]["fighters"] for k in FFIELDS},
        "drone_control_range": st["drones"]["control_range_m"],
    }

DFIELDS = ("optimal_m", "falloff_m", "tracking", "max_velocity", "signature_radius")
FFIELDS = ("max_velocity", "signature_radius")
WFIELDS = ("optimal_m", "falloff_m", "tracking", "range_m", "explosion_radius", "explosion_velocity")

def pyfa(s):
    out = {k: s[k] for k in ("cpu_used", "cpu_total", "power_used", "power_total", "calibration_used", "drone_bandwidth_used",
                             "weapon_dps", "weapon_volley", "drone_dps", "drone_volley", "cap_capacity", "cap_recharge_s", "cap_stable",
                             "max_velocity", "align_time_s", "mass", "signature_radius", "warp_speed", "max_targets", "max_target_range",
                             "scan_resolution", "scan_strength", "hi_slots", "med_slots", "low_slots", "jam_chance", "warp_scramble_status") if k in s}
    for l in ("shield", "armor", "hull"):
        out[f"hp.{l}"] = s["hp"][l]; out[f"ehp.{l}"] = s["ehp"][l]
        for k in ("em", "thermal", "kinetic", "explosive"):
            out[f"res.{l}.{k}"] = s["resonance"][l][k]
    t = s["tank"]
    out.update({"tank.armor": t["armorRepair"], "tank.shield": t["shieldRepair"], "tank.hull": t["hullRepair"], "tank.passive": t["passiveShield"]})
    for w in s.get("weapons", []):
        for k in WFIELDS:
            if k in w and w[k] is not None:
                out[f"w{w['module_index']}.{k}"] = w[k]
                PTR[f"w{w['module_index']}.{k}"] = f"/offense/weapons[module_index={w['module_index']}]/{k}"
    st_ = s.get("sustainable_tank")
    if st_:
        out.update({"stank.armor": st_["armorRepair"], "stank.shield": st_["shieldRepair"], "stank.hull": st_["hullRepair"]})
    for w in s.get("drones", []):
        for k in DFIELDS:
            if w.get(k) is not None:
                out[f"d{w['drone_index']}.{k}"] = w[k]
                PTR[f"d{w['drone_index']}.{k}"] = f"/offense/drones[drone_index={w['drone_index']}]/{k}"
    for w in s.get("fighters", []):
        for k in FFIELDS:
            if w.get(k) is not None:
                out[f"f{w['fighter_index']}.{k}"] = w[k]
                PTR[f"f{w['fighter_index']}.{k}"] = f"/offense/fighters[fighter_index={w['fighter_index']}]/{k}"
    if "drone_control_range" in s:
        out["drone_control_range"] = s["drone_control_range"]
    cs = s["cap_state"]
    out["cap_state"] = cs if s["cap_stable"] else cs
    return out

# metric -> JSON pointer into FitStats (used to generate tests/oracle/pyfa_expected.json for `cargo test`)
PTR = {
    "cpu_used": "/resources/cpu/used", "cpu_total": "/resources/cpu/total", "power_used": "/resources/power/used",
    "power_total": "/resources/power/total", "calibration_used": "/resources/calibration/used",
    "drone_bandwidth_used": "/resources/drone_bandwidth/used",
    **{f"hp.{l}": f"/defense/hp/{l}" for l in ("shield", "armor", "hull")},
    **{f"ehp.{l}": f"/defense/ehp/{l}" for l in ("shield", "armor", "hull")},
    **{f"res.{l}.{k}": f"/defense/resonance/{l}/{k}" for l in ("shield", "armor", "hull") for k in ("em", "thermal", "kinetic", "explosive")},
    "weapon_dps": "/offense/total/weapon_dps", "weapon_volley": "/offense/total/weapon_volley",
    "drone_dps": "/offense/total/drone_dps+/offense/total/fighter_dps",
    "drone_volley": "/offense/total/drone_volley+/offense/total/fighter_volley",
    "cap_capacity": "/capacitor/capacity", "cap_recharge_s": "/capacitor/recharge_time_s", "cap_stable": "/capacitor/stable",
    "max_velocity": "/navigation/max_velocity", "align_time_s": "/navigation/align_time_s", "mass": "/navigation/mass",
    "signature_radius": "/navigation/signature_radius", "warp_speed": "/navigation/warp_speed_au_s",
    "max_targets": "/targeting/max_targets", "max_target_range": "/targeting/max_range_m",
    "scan_resolution": "/targeting/scan_resolution", "scan_strength": "/targeting/sensor_strength",
    "jam_chance": "/targeting/jam_chance_percent", "drone_control_range": "/drones/control_range_m", "warp_scramble_status": "/navigation/warp_scramble_status",
    "tank.armor": "/defense/tank/raw/armor_repair", "tank.shield": "/defense/tank/raw/shield_repair",
    "stank.armor": "/defense/tank/sustained/armor_repair", "stank.shield": "/defense/tank/sustained/shield_repair",
    "stank.hull": "/defense/tank/sustained/hull_repair",
    "tank.hull": "/defense/tank/raw/hull_repair", "tank.passive": "/defense/tank/raw/passive_shield",
    "hi_slots": "/resources/slots/high/total", "med_slots": "/resources/slots/mid/total", "low_slots": "/resources/slots/low/total",
}

NSA = ("SDE: Networked Sensor Array ModAdd warpScrambleStatus += warpScrambleStrength (+100); Pyfa's hand-written "
       "moduleBonusNetworkedSensorArray omits it")
# Known, explained divergences (see PROGRESS.md / docs/03): metric skipped in the generated test expectations.
KNOWN = {
    "esf_items_4": {"max_velocity": "two prop mods active at once (invalid fit); Pyfa re-reads mass per handler"},
    "esf_items_7": {"cap_capacity": "structure module on a ship (invalid fit): Pyfa applies it, SDE domain says no"},
    "esf_projection_18": {"align_time_s": "data drift: Pyfa eve.db (client 3532181) Paladin agility 0.858 vs SDE 3569502 0.0858"},
    "exct_hel": {"warp_scramble_status": NSA}, "exct_nidhoggur": {"warp_scramble_status": NSA},
    "esf_structure_bonus_1": {"hp.armor": "SDE: unpowered structure zeroes plating bonus (ESF agrees); Pyfa hand-written handler ignores power state",
                              "ehp.armor": "same as hp.armor"},
}

def close(a, b):
    if isinstance(a, bool) or isinstance(b, bool): return bool(a) == bool(b)
    if a is None or b is None: return a == b
    return math.isclose(float(a), float(b), rel_tol=1e-4, abs_tol=1e-3)

PATCHES = {}

def main(files):
    reqs = []
    for f in files:
        name = pathlib.Path(f).stem
        if f.endswith(".json"):
            # {"eft": "fits/x.eft", "patch": {...}}  -> EFT + extra request fields (projections, damage pattern...)
            spec = json.loads(pathlib.Path(f).read_text())
            p = subprocess.run([BIN, "eft", str(ROOT / "tests" / spec["eft"]), "--skills", "5"], capture_output=True, text=True)
            if p.returncode: print(f"{name}: SKIP parse ({p.stderr.strip()})"); continue
            patch = spec.get("patch", {})
            if spec.get("booster_efts"):
                bfs = [json.loads(subprocess.run([BIN, "eft", str(ROOT / "tests" / e), "--skills", "5"], capture_output=True, text=True, check=True).stdout)
                       for e in spec["booster_efts"]]
                patch = {**patch, "fleet": {"buffs": [], "booster_fits": bfs}}
            for pe in spec.get("projected_efts", []):
                pf = json.loads(subprocess.run([BIN, "eft", str(ROOT / "tests" / pe["eft"]), "--skills", "5"], capture_output=True, text=True, check=True).stdout)
                patch = {**patch, "projected": patch.get("projected", []) + [{"kind": "fit", "fit": pf, "amount": pe.get("amount", 1), "distance_m": pe.get("distance_m")}]}
            req = json.loads(p.stdout); req.update(patch)
            PATCHES[name] = patch
            base = pathlib.Path(spec["eft"]).stem
            if base in KNOWN and name not in KNOWN:
                KNOWN[name] = KNOWN[base]  # same divergence in derived cases
        else:
            p = subprocess.run([BIN, "eft", f, "--skills", "5"], capture_output=True, text=True)
            if p.returncode: print(f"{name}: SKIP parse ({p.stderr.strip()})"); continue
            req = json.loads(p.stdout)
        if (req.get("fleet") or {}).get("buffs") or any(p.get("kind") not in ("module", "drone", "fit", "fighter") for p in req.get("projected", [])):
            print(f"{name}: SKIP (oracle lacks projection/fleet)"); continue
        rp = TMP / f"{name}.json"; rp.write_text(json.dumps(req)); reqs.append((name, rp, f))
    env = dict(os.environ, PYTHONPATH=f"{REF}/stubs", ORACLE_REPEAT="3")
    pr = subprocess.run([f"{REF}/pyfa-venv/bin/python", str(ROOT / "oracle/pyfa_oracle.py"), *[str(p) for _, p, _ in reqs]],
                        capture_output=True, text=True, cwd=f"{REF}/pyfa", env=env)
    orc = {json.loads(l)["file"][:-5]: json.loads(l) for l in pr.stdout.splitlines() if l.startswith("{")}
    if pr.returncode: print(pr.stderr[-3000:])
    total = ok = 0; report = {}; expected = {}
    for name, rp, f in reqs:
        if name not in orc or "error" in orc[name]: print(f"{name}: SKIP oracle error {orc.get(name, {}).get('error')}"); continue
        st = json.loads(subprocess.run([BIN, "calc", str(rp)], capture_output=True, text=True).stdout)
        a, b = ours(st), pyfa(orc[name]["stats"])
        bad = {k: (a.get(k), b[k]) for k in b if k != "cap_state" and not close(a[k], b[k])}
        if b["cap_stable"] and a["cap_stable"] and not close(a["cap_state"], b["cap_state"]): bad["cap_state"] = (a["cap_state"], b["cap_state"])
        for k in list(bad):
            if k in KNOWN.get(name, {}): bad.pop(k)
        expected[name] = {"eft": f"tests/fits/{name}.eft", **({"request_patch": PATCHES[name],
                          "eft": "tests/" + json.loads(pathlib.Path(f).read_text())["eft"]} if str(f).endswith(".json") else {}),
                          "values": {PTR[k]: v for k, v in b.items() if k in PTR and k not in KNOWN.get(name, {})},
                          **({"cap_state_percent": b["cap_state"]} if b["cap_stable"] else {})}
        total += 1; ok += not bad
        report[name] = {"mismatches": bad, "pyfa_ms": orc[name]["timing_ms"]}
        print(f"{name}: {'OK' if not bad else 'DIFF ' + json.dumps(bad)}")
    print(f"\n{ok}/{total} fits match Pyfa on all {len(b) if reqs else 0} compared metrics (known divergences excluded: {sum(len(v) for v in KNOWN.values())})")
    (ROOT / "tests/oracle").mkdir(exist_ok=True)
    if os.environ.get("WRITE_EXPECTED"):
        (ROOT / "tests/oracle/pyfa_expected.json").write_text(json.dumps({"generator": "oracle/compare.py (Pyfa eos as black-box)",
            "pyfa_client_build": 3532181, "skills": "all 5", "known_divergences": KNOWN, "fits": expected}, indent=1, sort_keys=True, default=str))
    (ROOT / "oracle/results").mkdir(exist_ok=True)
    (ROOT / "oracle/results/latest.json").write_text(json.dumps(report, indent=1, sort_keys=True, default=str))

if __name__ == "__main__":
    main(sys.argv[1:])
