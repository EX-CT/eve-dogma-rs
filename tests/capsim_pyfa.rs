//! Capacitor-simulation parity on fits that exercise (a) cycle times computed in floating point and floored to whole
//! milliseconds by the simulator (overheated modules: 7649.999.. ms -> 7649 ms) and (b) incoming void bombs.
//! Requests and expected values come from the eve-dogma-bench `cap-suite` (CONTRACT-CAP 0.1); the values were
//! produced by running Pyfa as a black-box oracle.
//! Needs the dataset: $EVE_DOGMA_DATASET or ./dataset.json.gz (skips otherwise, like oracle_parity).
use eve_dogma::{Dataset, FitRequest, calc};
use serde_json::Value;

fn dataset() -> Option<Dataset> {
    let p = std::env::var("EVE_DOGMA_DATASET").unwrap_or_else(|_| "dataset.json.gz".into());
    if !std::path::Path::new(&p).exists() {
        eprintln!("SKIP: dataset not found at {p}");
        return None;
    }
    Some(Dataset::load_path(&p).expect("load dataset"))
}

fn fixtures() -> Value {
    serde_json::from_str(&std::fs::read_to_string("tests/capsim/pyfa_cap_expected.json").unwrap()).unwrap()
}

fn run(ds: &Dataset, f: &Value) -> Value {
    let req: FitRequest = serde_json::from_value(f["request"].clone()).expect("request");
    calc(ds, &req)
}

/// CONTRACT-CAP 0.1 tolerances: max(1e-3, 1e-4 * |want|); depletion time within the same millisecond
fn close(metric: &str, got: &Value, want: &Value) -> bool {
    match (got, want) {
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Number(a), Value::Number(b)) => {
            let (a, b) = (a.as_f64().unwrap(), b.as_f64().unwrap());
            let tol = if metric == "depletes_in_s" { 0.0005 } else { 1e-3_f64.max(1e-4 * b.abs()) };
            (a - b).abs() <= tol
        }
        _ => false,
    }
}

#[test]
fn capacitor_matches_pyfa_cap_suite_subset() {
    let Some(ds) = dataset() else { return };
    let fx = fixtures();
    let mut failures = Vec::new();
    for (name, f) in fx["fits"].as_object().unwrap() {
        let st = run(&ds, f);
        for (metric, want) in f["values"].as_object().unwrap() {
            let got = st["capacitor"].get(metric).cloned().unwrap_or(Value::Null);
            if !close(metric, &got, want) {
                failures.push(format!("{name} {metric}: got {got} want {want}"));
            }
        }
    }
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn overheated_cycle_is_folded_like_pyfa() {
    // 12000 ms * (0.75 skill * 0.85 overheat) evaluates to 7649.999.. in doubles when the unpenalised multipliers are
    // folded into one product first (Pyfa's order); the simulator then floors it to 7649 ms.
    let Some(ds) = dataset() else { return };
    let fx = fixtures();
    let st = run(&ds, &fx["fits"]["oh_maller_reps"]);
    let reps: Vec<f64> = st["modules"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["name"] == "Medium Armor Repairer II")
        .map(|m| m["cycle_time_ms"].as_f64().unwrap())
        .collect();
    assert_eq!(reps.len(), 2);
    for c in reps {
        assert!(c < 7650.0 && c > 7649.99, "cycle {c}");
        assert_eq!(c.trunc(), 7649.0);
    }
}

#[test]
fn void_bomb_drains_capacitor() {
    let Some(ds) = dataset() else { return };
    let fx = fixtures();
    let with = run(&ds, &fx["fits"]["hard_void_bomb_bs"]);
    let mut req = fx["fits"]["hard_void_bomb_bs"]["request"].clone();
    req["projected"] = Value::Array(vec![]);
    let without = calc(&ds, &serde_json::from_value::<FitRequest>(req).unwrap());
    let used = |v: &Value| v["capacitor"]["use_gj_s"].as_f64().unwrap();
    assert!(used(&with) > used(&without) + 1.0, "void bomb adds cap use: {} vs {}", used(&with), used(&without));
    assert_eq!(with["capacitor"]["stable"], Value::Bool(false));
}
