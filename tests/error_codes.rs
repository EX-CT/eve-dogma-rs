//! Contract error codes for bad input: malformed JSON -> BAD_JSON, schema mismatch -> BAD_REQUEST.
use eve_dogma::request_error_code;

fn code(s: &str) -> &'static str {
    request_error_code(&serde_json::from_str::<eve_dogma::request::FitRequest>(s).unwrap_err())
}

#[test]
fn malformed_json_is_bad_json() {
    assert_eq!(code("{\"ship\": {\"type_id\": 587"), "BAD_JSON"); // truncated
    assert_eq!(code("{ship: 1}"), "BAD_JSON"); // not JSON
    assert_eq!(code(""), "BAD_JSON");
}

#[test]
fn schema_mismatch_is_bad_request() {
    assert_eq!(code("{\"ship\": {\"type_id\": \"rifter\"}}"), "BAD_REQUEST");
    assert_eq!(code("[1, 2]"), "BAD_REQUEST");
}

#[test]
fn calc_json_reports_bad_json() {
    let p = std::env::var("EVE_DOGMA_DATASET").unwrap_or_else(|_| "dataset.json.gz".into());
    if !std::path::Path::new(&p).exists() {
        return;
    }
    let ds = eve_dogma::data::Dataset::load_path(&p).expect("load dataset");
    let v: serde_json::Value = serde_json::from_str(&eve_dogma::calc_json(&ds, "{\"ship\":")).unwrap();
    assert_eq!(v["error"]["code"], "BAD_JSON");
    let v: serde_json::Value = serde_json::from_str(&eve_dogma::calc_json(&ds, "{\"ship\": {\"type_id\": \"x\"}}")).unwrap();
    assert_eq!(v["error"]["code"], "BAD_REQUEST");
}
