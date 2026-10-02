//! eve-dogma: stateless EVE Online fitting engine (EXCT).
//!
//! `calc(dataset, request) -> stats` is a pure function: no I/O, no clocks, no global state.
pub mod capsim;
pub mod data;
pub mod eft;
pub mod engine;
pub mod jout;
pub mod request;
pub mod stats;

use serde_json::{json, Value};

pub use data::Dataset;
pub use request::FitRequest;

/// Compute full fit statistics for one request.
pub fn calc(ds: &Dataset, req: &FitRequest) -> Value {
    match engine::Fit::build(ds, req) {
        Ok(fit) => fit.compute_stats(req).into_value(),
        Err(e) => json!({"error": {"code": e.code, "message": e.message, "path": e.path}}),
    }
}

/// Convenience: JSON string in, JSON string out.
pub fn calc_json(ds: &Dataset, request_json: &str) -> String {
    let v = match serde_json::from_str::<FitRequest>(request_json) {
        Ok(req) => match engine::Fit::build(ds, &req) {
            // written directly (same text as serialising `calc`'s Value)
            Ok(fit) => return fit.compute_stats(&req).to_json(),
            Err(e) => json!({"error": {"code": e.code, "message": e.message, "path": e.path}}),
        },
        Err(e) => json!({"error": {"code": "BAD_REQUEST", "message": e.to_string(), "path": ""}}),
    };
    serde_json::to_string(&v).unwrap()
}
