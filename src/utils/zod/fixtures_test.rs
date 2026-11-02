//! Cross-checks the carrier against golden fixtures produced by running real
//! zod v4 in the rebuild tree (`scripts/gen_zod_fixtures.mjs`).
//!
//! Each fixture fixes one schema + one input and records zod's own `data`,
//! `issues`, and `toJSONSchema` output. The carrier's `safe_parse` and
//! `to_json_schema` must reproduce them. This is the oracle the design calls
//! for: alignment is proven against zod's actual output, not against a reading
//! of the spec.

use super::error::{IssueCode, PathSegment};
use super::schema::*;
use super::{safe_parse, to_json_schema};
use serde_json::{Value, json};

const FIXTURES: &str = include_str!("fixtures.json");

fn fixture(name: &str) -> Value {
    let all: Value = serde_json::from_str(FIXTURES).expect("fixtures parse");
    all.as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no fixture named {name}"))
        .clone()
}

/// The schema each fixture was generated from, rebuilt with the carrier.
fn schema_for(name: &str) -> Schema {
    match name {
        "string_ok" | "string_wrong_type" => string(),
        "string_optional_null" => string().optional(),
        "default_absent" | "default_null_fails" => {
            object(vec![("a", string().default(json!("d")))])
        }
        "int_fract" | "int_over_safe" => number().int(),
        "nonnegative_neg" => number().nonnegative(),
        "positive_zero" => number().positive(),
        "enum_ok" | "enum_bad" => enumeration(vec!["a", "b"]),
        "literal_ok" | "literal_bad" => literal(json!("x")),
        "strict_stray" => strict_object(vec![("a", string())]),
        "object_strip" | "object_missing" => object(vec![("a", string())]),
        "passthrough_keeps_unknown" | "passthrough_still_requires" => {
            passthrough_object(vec![("a", string())])
        }
        "passthrough_empty_shape" => passthrough_object(vec![]),
        "string_min_fail" | "string_min_ok" => string().min(2),
        "string_regex_fail" | "string_regex_ok" => string().regex(r"^[\w-]+$"),
        "string_url_fail" | "string_url_ok" => string().url(),
        "array_min_fail" => array(string()).min(1),
        "array_max_fail" => array(string()).max(2),
        "array_minmax_ok" => array(string()).min(1).max(2),
        "num_min_fail" => number().min(0),
        "num_max_fail" => number().max(600_000),
        "num_minmax_ok" => number().min(0).max(600_000).default(json!(30000)),
        "string_min_custom_message" => string().min_with_message(1, "Content cannot be empty"),
        "string_length_short" | "string_length_long" | "string_length_ok" => string().length(40),
        "string_starts_with_fail" | "string_starts_with_ok" => string().starts_with("./"),
        "gitsha_combo_fail" => string().length(40).regex_with_message(
            r"^[a-f0-9]{40}$",
            "Must be a full 40-character lowercase git commit SHA",
        ),
        "refine_chain_both_fail" => string()
            .refine(
                |v| v.as_str().is_some_and(|s| !s.contains(' ')),
                "no spaces",
            )
            .refine(
                |v| v.as_str().is_some_and(|s| s == s.to_lowercase()),
                "lowercase only",
            ),
        "min_then_refine_fail" => string().min_with_message(5, "too short").refine(
            |v| v.as_str().is_some_and(|s| !s.contains(' ')),
            "no spaces",
        ),
        "type_fail_skips_refine" => {
            string().refine(|v| v.as_str().is_some_and(|s| s.len() > 1), "long enough")
        }
        "object_field_fail_skips_refine" => object(vec![("a", string().min(3))]).refine(
            |v| v.get("a").and_then(Value::as_str) != Some("zz"),
            "no zz",
        ),
        "partial_record_ok" | "partial_record_bad_key" | "partial_record_bad_value" => {
            partial_record(vec!["A", "B"], array(string()))
        }
        "super_refine_params" => string().super_refine(|v| {
            let val = v.as_str()?;
            (val.chars().count() < 3).then(|| SuperRefineIssue {
                message: "too short".to_string(),
                params: Some(json!({"received": val})),
            })
        }),
        "nested_path" => object(vec![("a", object(vec![("b", array(string()))]))]),
        "array_ok" => array(number()),
        "record_ok" => record(number()),
        "union_fail" => union(vec![string(), number()]),
        "disc_union_ok" => discriminated_union(
            "type",
            vec![
                strict_object(vec![
                    ("type", literal(json!("text"))),
                    ("file", strict_object(vec![("filePath", string())])),
                ]),
                strict_object(vec![
                    ("type", literal(json!("image"))),
                    ("file", strict_object(vec![("originalSize", number())])),
                ]),
            ],
        ),
        "disc_union_bad_tag" => discriminated_union(
            "type",
            vec![
                strict_object(vec![("type", literal(json!("a")))]),
                strict_object(vec![("type", literal(json!("b")))]),
            ],
        ),
        "read_input_projection" => strict_object(vec![
            (
                "file_path",
                string().describe("The absolute path to the file to read"),
            ),
            (
                "offset",
                number()
                    .int()
                    .nonnegative()
                    .optional()
                    .describe("The line number to start reading from"),
            ),
            (
                "limit",
                number()
                    .int()
                    .positive()
                    .optional()
                    .describe("The number of lines to read"),
            ),
        ]),
        "any_passthrough" => any(),
        "refine_ok" | "refine_fail" => {
            string().refine(|v| v.as_str().is_some_and(|s| s.len() > 3), "too short")
        }
        "transform_data" => string().transform(|v| json!(v.as_str().unwrap().len())),
        "nullable_null" | "nullable_wrong_type" => string().nullable(),
        "optional_default_absent" => {
            object(vec![("r", boolean().default(json!(false)).optional())])
        }
        "strict_multi_issue" => strict_object(vec![("a", string()), ("b", number())]),
        "array_multi_issue" => array(number()),
        other => panic!("no schema mapping for fixture {other}"),
    }
}

fn fixture_input(fx: &Value) -> Value {
    // `__undefined__` marks an absent value; no fixture currently needs it for
    // a present-vs-absent distinction at the top level, so treat it as null.
    match &fx["input"] {
        Value::Object(m) if m.contains_key("__undefined__") => Value::Null,
        v => v.clone(),
    }
}

/// Every fixture's `toJSONSchema` must be reproduced by the carrier. Compares
/// content key-order-insensitively: zod writes its own key order and the
/// carrier preserves declaration order — what must match is the set of keys
/// and values at every level, not their serialization order.
#[test]
fn json_schema_matches_zod_v4_for_every_fixture() {
    let all: Value = serde_json::from_str(FIXTURES).expect("fixtures parse");
    for case in all.as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let expected = &case["json_schema"];
        if expected.get("__error__").is_some() {
            continue; // schema zod itself could not project
        }
        // The draft marker is part of the projection, `$schema` included: no
        // caller attaches it, and `api.ts:160` ships this object verbatim.
        let expected = expected.clone();
        let got = to_json_schema(&schema_for(name));
        assert!(
            json_content_eq(&got, &expected),
            "json_schema mismatch for {name}\n  got:  {got}\n  want: {expected}"
        );
    }
}

/// Content equality: objects compare key-sets and recurse; arrays compare in
/// order; scalars compare by value (numbers by their f64, since fixtures come
/// from JSON text where 30 and 30.0 are distinct only in representation).
fn json_content_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| json_content_eq(v, w)))
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(v, w)| json_content_eq(v, w))
        }
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => a == b,
    }
}

/// Successful fixtures: the transformed `data` must match.
#[test]
fn safe_parse_data_matches_zod_v4_on_success() {
    let all: Value = serde_json::from_str(FIXTURES).expect("fixtures parse");
    for case in all.as_array().unwrap() {
        if !case["success"].as_bool().unwrap_or(false) {
            continue;
        }
        let name = case["name"].as_str().unwrap();
        let got = safe_parse(&schema_for(name), &fixture_input(case))
            .unwrap_or_else(|e| panic!("{name} should succeed, got {e:?}"));
        assert_eq!(got, case["data"], "data mismatch for {name}");
    }
}

/// Failed fixtures: the issue codes and paths must match. Message text is
/// checked only where it is load-bearing (the `received undefined` marker);
/// full copy alignment is `format_zod_validation_error`'s job.
#[test]
fn safe_parse_issue_codes_and_paths_match_zod_v4_on_failure() {
    let all: Value = serde_json::from_str(FIXTURES).expect("fixtures parse");
    for case in all.as_array().unwrap() {
        if case["success"].as_bool().unwrap_or(true) {
            continue;
        }
        let name = case["name"].as_str().unwrap();
        let err = match safe_parse(&schema_for(name), &fixture_input(case)) {
            Ok(d) => panic!("{name} should fail, got data {d:?}"),
            Err(e) => e,
        };
        let expected_issues = case["issues"].as_array().unwrap();
        assert_eq!(
            err.issues.len(),
            expected_issues.len(),
            "issue count mismatch for {name}: {err:?}"
        );
        for (got, want) in err.issues.iter().zip(expected_issues.iter()) {
            assert_eq!(
                got.code.as_str(),
                want["code"].as_str().unwrap(),
                "issue code mismatch for {name}"
            );
            // Verbatim message — this is what the model reads via the fallback.
            assert_eq!(
                got.message,
                want["message"].as_str().unwrap(),
                "issue message mismatch for {name}"
            );
            // Path as JSON (keys are strings, indices are numbers).
            let got_path: Vec<Value> = got
                .path
                .iter()
                .map(|seg| match seg {
                    PathSegment::Key(k) => json!(k),
                    PathSegment::Index(i) => json!(i),
                })
                .collect();
            assert_eq!(
                Value::Array(got_path),
                want["path"],
                "issue path mismatch for {name}"
            );
        }
    }
}

#[test]
fn missing_required_param_marks_received_undefined() {
    let fx = fixture("object_missing");
    let err = safe_parse(&schema_for("object_missing"), &fixture_input(&fx)).unwrap_err();
    assert_eq!(err.issues[0].code, IssueCode::InvalidType);
    assert!(err.issues[0].message.contains("received undefined"));
    assert_eq!(err.issues[0].path, vec![PathSegment::Key("a".to_string())]);
}

// refine / transform / nullable — behaviours sampled from real zod v4
// (see the conversation that added them; not yet in fixtures.json).

#[test]
fn refine_fails_with_a_custom_issue_like_official() {
    // zod oracle: `{"code":"custom","message":"too short"}`.
    let schema = string().refine(|v| v.as_str().is_some_and(|s| s.len() > 3), "too short");
    let err = safe_parse(&schema, &json!("ab")).unwrap_err();
    assert_eq!(err.issues[0].code, IssueCode::Custom);
    assert_eq!(err.issues[0].message, "too short");
    // and passes through when the predicate holds
    assert_eq!(safe_parse(&schema, &json!("abcd")).unwrap(), json!("abcd"));
    // refine is invisible in the projection (oracle: refine.schema → inner string)
    assert_eq!(to_json_schema(&schema)["type"], json!("string"));
}

#[test]
fn transform_changes_the_data_and_refuses_projection_like_official() {
    // zod oracle: data 5; toJSONSchema throws "Transforms cannot be represented".
    let schema = string().transform(|v| json!(v.as_str().unwrap().len()));
    assert_eq!(safe_parse(&schema, &json!("hello")).unwrap(), json!(5));
    assert_eq!(
        to_json_schema(&schema)["$error"],
        json!("Transforms cannot be represented in JSON Schema")
    );
}

#[test]
fn nullable_accepts_null_and_rejects_absent_inner_type_mismatch() {
    // zod oracle: null passes, projection is anyOf[inner, {type:null}],
    // a wrong non-null type fails with the inner type error.
    let schema = string().nullable();
    assert_eq!(safe_parse(&schema, &json!(null)).unwrap(), json!(null));
    let err = safe_parse(&schema, &json!(5)).unwrap_err();
    assert_eq!(err.issues[0].code, IssueCode::InvalidType);
    assert_eq!(err.issues[0].expected, Some("string"));
    let projected = to_json_schema(&schema);
    assert_eq!(projected["anyOf"][0]["type"], json!("string"));
    assert_eq!(projected["anyOf"][1]["type"], json!("null"));
}
