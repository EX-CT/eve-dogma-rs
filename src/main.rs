//! eve-dogma CLI — stateless: JSON FitRequest in, JSON FitStats out.

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
use eve_dogma::{calc, eft, Dataset, FitRequest};
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::time::Instant;

const USAGE: &str = "eve-dogma <command> [--dataset PATH] [args]

Commands:
  calc [FILE]            FitRequest JSON (file or stdin) -> FitStats JSON
  batch                  JSONL FitRequests on stdin -> JSONL FitStats on stdout
  serve-stdio            JSONL RPC: {\"id\":..,\"method\":\"calc|eft_parse|eft_export|search|type|meta\",\"params\":..}
  eft [FILE]             EFT text (file or stdin) -> FitRequest JSON (add --calc to compute, --skills N)
  search QUERY           search types by name
  type ID|NAME           show type with base attributes
  meta                   dataset info
  bench [FILE] [-n N]    time N calculations of a request

Dataset: --dataset PATH, or $EVE_DOGMA_DATASET, or ./dataset.json.gz";

/// the dataset lives for the whole process: leaked so exit skips freeing ~10k types (~5% of a cold calc)
fn load(path: Option<String>) -> &'static Dataset {
    let p = path.or_else(|| std::env::var("EVE_DOGMA_DATASET").ok()).unwrap_or_else(|| "dataset.json.gz".into());
    match Dataset::load_path(&p) {
        Ok(d) => Box::leak(Box::new(d)),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(3)
        }
    }
}

fn read_input(file: Option<&String>) -> String {
    let mut s = String::new();
    match file {
        Some(f) if f != "-" => s = std::fs::read_to_string(f).unwrap_or_else(|e| {
            eprintln!("error: {f}: {e}");
            std::process::exit(2)
        }),
        _ => {
            std::io::stdin().read_to_string(&mut s).unwrap();
        }
    }
    s
}

/// Interim search spec (contract v1.4.1): published types of the scored categories, rank exact > prefix > substring
/// (case-insensitive, English or Chinese name), ties by type id ascending; default limit 20.
const SEARCH_CATEGORIES: [(&str, u32); 8] =
    [("ship", 6), ("module", 7), ("charge", 8), ("drone", 18), ("fighter", 87), ("implant", 20), ("subsystem", 32), ("skill", 16)];

fn search_kind(ds: &Dataset, t: &eve_dogma::data::TypeInfo) -> Option<&'static str> {
    if t.category == 20 {
        let booster = ds.groups.get(&t.group).map(|g| g.name.contains("Booster")).unwrap_or(false);
        return Some(if booster { "booster" } else { "implant" });
    }
    SEARCH_CATEGORIES.iter().find(|(_, c)| *c == t.category).map(|(n, _)| *n)
}

fn search(ds: &Dataset, q: &str, limit: usize, kinds: Option<Vec<String>>) -> Value {
    let ql = q.trim().to_lowercase();
    let rank = |id: &u32, t: &eve_dogma::data::TypeInfo| -> Option<u8> {
        let en = t.name.to_lowercase();
        let zh = ds.name_zh(*id).map(|z| z.to_lowercase());
        let zh = zh.as_deref().unwrap_or("");
        if en == ql || (!zh.is_empty() && zh == ql) {
            Some(0)
        } else if en.starts_with(&ql) || (!zh.is_empty() && zh.starts_with(&ql)) {
            Some(1)
        } else if en.contains(&ql) || (!zh.is_empty() && zh.contains(&ql)) {
            Some(2)
        } else {
            None
        }
    };
    let mut hits: Vec<(u8, u32, &'static str, &eve_dogma::data::TypeInfo)> = ds
        .types
        .iter()
        .filter(|(_, t)| t.published)
        .filter_map(|(id, t)| {
            let k = search_kind(ds, t)?;
            if let Some(ks) = &kinds {
                if !ks.iter().any(|x| x == k) {
                    return None;
                }
            }
            Some((rank(id, t)?, *id, k, t))
        })
        .collect();
    hits.sort_by_key(|h| (h.0, h.1));
    Value::Array(
        hits.into_iter()
            .take(limit)
            .map(|(r, id, k, t)| {
                json!({"type_id": id, "name": t.name, "name_zh": ds.name_zh(id), "kind": k, "match": (["exact", "prefix", "substring"][r as usize]),
                       "group": ds.groups.get(&t.group).map(|g| g.name.clone()), "category_id": t.category,
                       "meta_level": t.meta_level, "slot": eve_dogma::engine::infer_slot(ds, t)})
            })
            .collect(),
    )
}

fn type_info(ds: &Dataset, key: &str) -> Value {
    let id = key.parse::<u32>().ok().or_else(|| ds.type_by_name(key));
    let Some(t) = id.and_then(|i| ds.types.get(&i)) else { return json!({"error": {"code": "UNKNOWN_TYPE", "message": key}}) };
    let attrs: serde_json::Map<String, Value> = t
        .attrs
        .iter()
        .map(|(a, v)| (ds.attrs.get(a).map(|x| x.name.clone()).unwrap_or(a.to_string()), json!(v)))
        .collect();
    let effects: Vec<Value> = t.effects.iter().map(|(e, d)| json!({"id": e, "name": ds.effects.get(e).map(|x| x.name.clone()), "default": d})).collect();
    json!({"type_id": t.id, "name": t.name, "name_zh": ds.name_zh(t.id), "group": ds.groups.get(&t.group).map(|g| g.name.clone()), "group_id": t.group,
           "category_id": t.category, "published": t.published, "mass": t.mass, "volume": t.volume, "capacity": t.capacity,
           "slot": eve_dogma::engine::infer_slot(ds, t), "attributes": attrs, "effects": effects})
}

fn meta(ds: &Dataset) -> Value {
    json!({"engine": concat!("eve-dogma-rs ", env!("CARGO_PKG_VERSION")), "schema_version": 1, "sde_build": ds.build,
           "sde_release_date": ds.release_date, "dataset_sha256": ds.sha256, "types": ds.types.len(), "attributes": ds.attrs.len(), "effects": ds.effects.len()})
}

fn rpc(ds: &Dataset, line: &str) -> Value {
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return json!({"id": null, "error": {"code": "BAD_JSON", "message": e.to_string()}}),
    };
    let id = v.get("id").cloned().unwrap_or(Value::Null);
    let p = v.get("params").cloned().unwrap_or(Value::Null);
    let result = match v.get("method").and_then(|m| m.as_str()).unwrap_or("calc") {
        "calc" => match serde_json::from_value::<FitRequest>(p) {
            Ok(r) => calc(ds, &r),
            Err(e) => json!({"error": {"code": "BAD_REQUEST", "message": e.to_string()}}),
        },
        "eft_parse" => match eft::parse(ds, p.get("text").and_then(|t| t.as_str()).unwrap_or("")) {
            Ok(r) => serde_json::to_value(r).unwrap(),
            Err(e) => json!({"error": {"code": "EFT_PARSE", "message": e}}),
        },
        "eft_export" => match serde_json::from_value::<FitRequest>(p.get("fit").cloned().unwrap_or(Value::Null)) {
            Ok(r) => json!({"text": eft::export(ds, &r, p.get("name").and_then(|n| n.as_str()).unwrap_or("EXCT fit"))}),
            Err(e) => json!({"error": {"code": "BAD_REQUEST", "message": e.to_string()}}),
        },
        "search" => search(
            ds,
            p.get("query").and_then(|q| q.as_str()).unwrap_or(""),
            p.get("limit").and_then(|l| l.as_u64()).unwrap_or(20) as usize,
            p.get("kinds").and_then(|k| k.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()),
        ),
        "type" => type_info(ds, &p.get("id").map(|x| x.to_string().trim_matches('"').to_string()).unwrap_or_default()),
        "meta" => meta(ds),
        m => json!({"error": {"code": "UNKNOWN_METHOD", "message": m}}),
    };
    json!({"id": id, "result": result})
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut dataset = None;
    if let Some(p) = args.iter().position(|a| a == "--dataset") {
        dataset = args.get(p + 1).cloned();
        args.drain(p..(p + 2).min(args.len()));
    }
    let take_flag = |args: &mut Vec<String>, f: &str| -> Option<String> {
        let p = args.iter().position(|a| a == f)?;
        let v = args.get(p + 1).cloned();
        args.drain(p..(p + 2).min(args.len()));
        v
    };
    let cmd = args.first().cloned().unwrap_or_default();
    let out = std::io::stdout();
    let mut out = out.lock();
    match cmd.as_str() {
        "calc" => {
            let ds = load(dataset);
            let s = read_input(args.get(1));
            let res = eve_dogma::calc_json(&ds, &s);
            writeln!(out, "{res}").unwrap();
            if res.starts_with("{\"error\"") {
                std::process::exit(2);
            }
        }
        "batch" => {
            let ds = load(dataset);
            for line in std::io::stdin().lock().lines() {
                let line = line.unwrap();
                if line.trim().is_empty() {
                    continue;
                }
                writeln!(out, "{}", eve_dogma::calc_json(&ds, &line)).unwrap();
            }
        }
        "serve-stdio" => {
            let ds = load(dataset);
            eprintln!("eve-dogma serve-stdio ready (sde {})", ds.build);
            for line in std::io::stdin().lock().lines() {
                let line = line.unwrap();
                if line.trim().is_empty() {
                    continue;
                }
                writeln!(out, "{}", serde_json::to_string(&rpc(&ds, &line)).unwrap()).unwrap();
                out.flush().unwrap();
            }
        }
        "eft" => {
            let skills = take_flag(&mut args, "--skills");
            let do_calc = args.iter().any(|a| a == "--calc");
            args.retain(|a| a != "--calc");
            let ds = load(dataset);
            let s = read_input(args.get(1));
            match eft::parse(&ds, &s) {
                Ok(mut r) => {
                    if let Some(l) = skills {
                        r.character.skills.default_level = l.parse().ok();
                    }
                    let v = if do_calc { calc(&ds, &r) } else { serde_json::to_value(&r).unwrap() };
                    writeln!(out, "{}", serde_json::to_string_pretty(&v).unwrap()).unwrap();
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(2)
                }
            }
        }
        "search" => {
            let ds = load(dataset);
            writeln!(out, "{}", serde_json::to_string_pretty(&search(&ds, &args[1..].join(" "), 20, None)).unwrap()).unwrap();
        }
        "type" => {
            let ds = load(dataset);
            writeln!(out, "{}", serde_json::to_string_pretty(&type_info(&ds, &args[1..].join(" "))).unwrap()).unwrap();
        }
        "meta" => {
            let t0 = Instant::now();
            let ds = load(dataset);
            let mut m = meta(&ds);
            m["load_ms"] = json!(t0.elapsed().as_secs_f64() * 1000.0);
            writeln!(out, "{}", serde_json::to_string_pretty(&m).unwrap()).unwrap();
        }
        "bench" => {
            let n: usize = take_flag(&mut args, "-n").and_then(|v| v.parse().ok()).unwrap_or(1000);
            let t0 = Instant::now();
            let ds = load(dataset);
            let load_ms = t0.elapsed().as_secs_f64() * 1000.0;
            let s = read_input(args.get(1));
            let req: FitRequest = serde_json::from_str(&s).expect("bad request");
            let _ = calc(&ds, &req);
            let t1 = Instant::now();
            for _ in 0..n {
                std::hint::black_box(calc(&ds, &req));
            }
            let el = t1.elapsed().as_secs_f64();
            writeln!(out, "{}", json!({"dataset_load_ms": load_ms, "iterations": n, "total_s": el, "per_calc_us": el / n as f64 * 1e6})).unwrap();
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(if cmd.is_empty() || cmd == "help" || cmd == "--help" { 0 } else { 2 });
        }
    }
}
