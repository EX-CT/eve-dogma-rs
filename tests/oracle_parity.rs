//! Parity test: every fit in tests/oracle/pyfa_expected.json (values produced by running Pyfa's eos
//! engine as a black-box oracle, see oracle/compare.py) must match eve-dogma within tolerance.
//! Needs the dataset: $EVE_DOGMA_DATASET or ./dataset.json.gz (CI downloads the release asset).
use eve_dogma::{Dataset, calc, eft};
use serde_json::Value;

fn dataset() -> Option<Dataset> {
    let p = std::env::var("EVE_DOGMA_DATASET").unwrap_or_else(|_| "dataset.json.gz".into());
    if !std::path::Path::new(&p).exists() {
        eprintln!("SKIP: dataset not found at {p}");
        return None;
    }
    Some(Dataset::load_path(&p).expect("load dataset"))
}

fn close(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => {
            let (x, y) = (x.as_f64().unwrap(), y.as_f64().unwrap());
            (x - y).abs() <= 1e-3_f64.max(1e-4 * y.abs())
        }
        (Value::Number(x), Value::Bool(y)) | (Value::Bool(y), Value::Number(x)) => (x.as_f64().unwrap() != 0.0) == *y,
        _ => a == b,
    }
}

#[test]
fn matches_pyfa_oracle() {
    let Some(ds) = dataset() else { return };
    let exp: Value = serde_json::from_str(&std::fs::read_to_string("tests/oracle/pyfa_expected.json").unwrap()).unwrap();
    let fits = exp["fits"].as_object().unwrap();
    let mut failures = Vec::new();
    let mut checked = 0;
    for (name, f) in fits {
        let text = std::fs::read_to_string(f["eft"].as_str().unwrap()).unwrap();
        let mut req = eft::parse(&ds, &text).unwrap_or_else(|e| panic!("{name}: {e}"));
        req.character.skills.default_level = Some(5);
        if let Some(patch) = f.get("request_patch").and_then(|p| p.as_object()) {
            let mut v = serde_json::to_value(&req).unwrap();
            for (k, x) in patch {
                v[k] = x.clone();
            }
            req = serde_json::from_value(v).unwrap_or_else(|e| panic!("{name}: bad patch {e}"));
        }
        let st = calc(&ds, &req);
        for (ptr, want) in f["values"].as_object().unwrap() {
            checked += 1;
            // "a+b" sums several pointers (Pyfa's drone stats include fighters)
            let got = if ptr.contains('+') {
                Value::from(ptr.split('+').map(|p| lookup(&st, p).and_then(|v| v.as_f64()).unwrap_or(0.0)).sum::<f64>())
            } else {
                lookup(&st, ptr).unwrap_or(Value::Null)
            };
            if !close(&got, want) {
                failures.push(format!("{name} {ptr}: got {got} want {want}"));
            }
        }
        if let Some(cs) = f.get("cap_state_percent") {
            checked += 1;
            let got = st.pointer("/capacitor/stable_percent").cloned().unwrap_or(Value::Null);
            if !close(&got, cs) {
                failures.push(format!("{name} cap: got {got} want {cs}"));
            }
        }
    }
    eprintln!("checked {checked} values over {} fits", fits.len());
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn deterministic_output() {
    let Some(ds) = dataset() else { return };
    let text = std::fs::read_to_string("tests/fits/esf_vexor.eft").unwrap();
    let mut req = eft::parse(&ds, &text).unwrap();
    req.character.skills.default_level = Some(5);
    let a = serde_json::to_string(&calc(&ds, &req)).unwrap();
    let b = serde_json::to_string(&calc(&ds, &req)).unwrap();
    assert_eq!(a, b);
}

#[test]
fn eft_roundtrip_with_mutations() {
    let Some(ds) = dataset() else { return };
    let text = std::fs::read_to_string("tests/fits/esf_mutations.eft").unwrap();
    let req = eft::parse(&ds, &text).unwrap();
    assert!(req.modules[0].mutation.is_some());
    let out = eft::export(&ds, &req, "Mutations");
    let req2 = eft::parse(&ds, &out).unwrap();
    assert_eq!(serde_json::to_value(&req.modules).unwrap(), serde_json::to_value(&req2.modules).unwrap());
    assert_eq!(serde_json::to_value(&req.drones).unwrap(), serde_json::to_value(&req2.drones).unwrap());
}

/// JSON pointer with an optional array selector segment `name[key=value]` (e.g. `/offense/weapons[module_index=3]/tracking`).
fn lookup(v: &Value, ptr: &str) -> Option<Value> {
    let mut cur = v.clone();
    for seg in ptr.split('/').skip(1) {
        if let (Some(b), true) = (seg.find('['), seg.ends_with(']')) {
            let (name, sel) = (&seg[..b], &seg[b + 1..seg.len() - 1]);
            let (k, want) = sel.split_once('=')?;
            let arr = cur.get(name)?.as_array()?.clone();
            cur = arr.into_iter().find(|e| e.get(k).map(|x| x.to_string() == want).unwrap_or(false))?;
        } else {
            cur = cur.get(seg)?.clone();
        }
    }
    Some(cur)
}
