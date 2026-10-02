#!/usr/bin/env python3
"""Pyfa EFT-export oracle (GPL-3.0-or-later, test tool only): builds each FitRequest JSON in Pyfa,
recalculates, fill()s empty slots (as Pyfa's GUI does before export) and prints
{"file", "name", "text"} per line using Pyfa's own exportEft with all options on.
usage: python pyfa_eft_export.py req.json [...]   (name defaults to the file stem)"""
import json, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import pyfa_oracle as po  # noqa: E402  (sets up Pyfa paths/db)
import types, importlib.util  # noqa: E402
from service.const import PortEftOptions  # noqa: E402

# Pyfa's service.port package drags in the wx GUI/ESI stack. Load eft.py and muta.py straight from
# source with tiny stand-ins for the GUI-only imports; the exporter code itself runs unmodified.
# The Market stand-in mirrors Market.getMarketGroupByItem / getGroupByItem for published items.


class _Market:
    @staticmethod
    def getInstance():
        return _Market

    @staticmethod
    def getMarketGroupByItem(item, parentcheck=True):
        if item.marketGroupID:
            return item.marketGroup
        if parentcheck and getattr(item, "varParent", None) is not None:
            return _Market.getMarketGroupByItem(item.varParent, False)
        return None

    @staticmethod
    def getGroupByItem(item):
        return item.group


def _stub(name, **attrs):
    m = types.ModuleType(name)
    m.__dict__.update(attrs)
    sys.modules[name] = m
    return m


_stub("service.market", Market=_Market)
_stub("service.esiAccess", EsiAccess=object)
_stub("service.fit", Fit=object)
_stub("gui.fitCommands.helpers", activeStateLimit=lambda *a, **k: None)
_stub("service.port", __path__=[])
_stub("service.port.shared", fetchItem=lambda name: None)


def _load(modname, rel):
    spec = importlib.util.spec_from_file_location(modname, os.path.join(po.PYFA, rel))
    mod = importlib.util.module_from_spec(spec)
    sys.modules[modname] = mod
    spec.loader.exec_module(mod)
    return mod


_load("service.port.muta", "service/port/muta.py")
exportEft = _load("service.port.eft", "service/port/eft.py").exportEft

OPTS = {o: True for o in PortEftOptions}

for path in sys.argv[1:]:
    req = json.load(open(path))
    name = os.path.splitext(os.path.basename(path))[0]
    try:
        fit = po.build(req)
        fit.name = name
        from eos.saveddata.cargo import Cargo
        for c in req.get("cargo", []):  # pyfa_oracle.build skips cargo (no stat impact)
            cg = Cargo(po.item(c["type_id"]))
            cg.amount = c.get("quantity", 1)
            fit.cargo.append(cg)
        fit.calculateModifiedAttributes()
        fit.fill()
        text = exportEft(fit, OPTS, None)
        print(json.dumps({"file": os.path.basename(path), "name": name, "text": text}))
    except Exception as e:
        try:
            po.eos.db.saveddata_session.rollback()
        except Exception:
            pass
        print(json.dumps({"file": os.path.basename(path), "name": name, "error": repr(e)}))
