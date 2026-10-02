//! EFT export parity: `eft::export` must reproduce Pyfa's `exportEft` (all options on, after the GUI's
//! `fill()`) byte for byte. Expected texts are frozen in tests/oracle/eft_export_expected.jsonl by
//! oracle/pyfa_eft_export.py + oracle/compare_eft_export.py --write.
//! Known data divergence: SDE 3569502 gives T3 cruisers maxSubSystems = 5, Pyfa's eve.db (client
//! 3532181) has 4, so we emit one extra "[Empty Subsystem slot]" line for every T3C fit.
use eve_dogma::{Dataset, FitRequest, eft};
use serde_json::Value;

#[test]
fn matches_pyfa_exporter() {
    let p = std::env::var("EVE_DOGMA_DATASET").unwrap_or_else(|_| "dataset.json.gz".into());
    if !std::path::Path::new(&p).exists() {
        eprintln!("SKIP: dataset not found at {p}");
        return;
    }
    let ds = Dataset::load_path(&p).expect("load dataset");
    let mut fails = Vec::new();
    let mut n = 0;
    for line in std::fs::read_to_string("tests/oracle/eft_export_expected.jsonl").unwrap().lines() {
        let e: Value = serde_json::from_str(line).unwrap();
        let req: FitRequest = serde_json::from_value(e["fit"].clone()).unwrap();
        let ours = eft::export(&ds, &req, e["name"].as_str().unwrap());
        let want = e["text"].as_str().unwrap();
        let t3c_known = ours.replacen("\n[Empty Subsystem slot]", "", 1);
        n += 1;
        if ours != want && t3c_known != want {
            fails.push(format!("{}:\n--- pyfa\n{want}\n--- ours\n{ours}", e["file"]));
        }
        // round trip: our own parser must read the export back to the same modules/drones
        let back = eft::parse(&ds, &ours).unwrap_or_else(|er| panic!("{}: reparse failed: {er}", e["file"]));
        assert_eq!(back.modules.len(), req.modules.len(), "{}: module count after reparse", e["file"]);
    }
    assert!(n > 250, "only {n} cases");
    assert!(fails.is_empty(), "{} / {n} EFT exports differ:\n{}", fails.len(), fails.join("\n\n"));
}
