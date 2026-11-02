//! `safe_parse`: validate a JSON value against a schema and return the
//! transformed data.
//!
//! Maps to: zod/v4 `schema.safeParse(input)` consumed at
//! `toolExecution.ts:615-686`. Success returns the transformed `data` —
//! strip / preprocess / default applied — never the input. Failure collects
//! ALL issues (zod/v4 does not short-circuit), with `unrecognized_keys` last.
//!
//! Behavioural rules taken from the zod/v4 oracle, not from memory:
//!   - An absent object key runs its field schema against `undefined`; the key
//!     is dropped only if that yields undefined (`.optional()`), filled if it
//!     yields a value (`.default()`, a wrapping `.transform()`). This is what
//!     makes FileEditTool's `replace_all` always defined at runtime.
//!   - `.optional()` accepts undefined, NOT explicit null; `.nullable()`
//!     accepts explicit null, NOT undefined.
//!   - preprocess transforms before validation; refine validates then checks;
//!     transform validates then maps.
//!   - `.int()` accepts up to the safe-integer bounds; past them the code is
//!     `too_big` / `too_small` by sign, a fraction is `invalid_type` "int".

use super::Value;
use super::error::{Issue, IssueCode, PathSegment, ZodError};
use super::schema::Schema;

/// Maps to: zod/v4 `schema.safeParse(input)` → `{ success, data }`.
pub fn safe_parse(schema: &Schema, input: &Value) -> Result<Value, ZodError> {
    let mut issues = Vec::new();
    match parse_at(schema, Some(input), &mut Vec::new(), &mut issues) {
        Ok(Some(data)) if issues.is_empty() => Ok(data),
        _ => Err(ZodError::new(issues)),
    }
}

/// `Ok(Some(v))` a value; `Ok(None)` the field resolved to undefined (drop the
/// key); `Err(())` a hard failure with the issue(s) pushed onto `issues`.
fn parse_at(
    schema: &Schema,
    input: Option<&Value>,
    path: &mut Vec<PathSegment>,
    issues: &mut Vec<Issue>,
) -> Result<Option<Value>, ()> {
    match schema {
        Schema::Preprocess(f, inner) => {
            // zod runs the preprocessor on `undefined` too; the transform must
            // pass it through untouched so the inner optional/default decides.
            // Real input transforms; absent input stays absent.
            match input {
                None => parse_at(inner, None, path, issues),
                Some(v) => {
                    let transformed = f(v);
                    parse_at(inner, Some(&transformed), path, issues)
                }
            }
        }
        Schema::Describe(inner, _) => parse_at(inner, input, path, issues),
        Schema::Optional(inner) => {
            // zod passes `undefined` THROUGH to the inner schema. If the inner
            // resolves it (a `.default()` fills, a nested `.optional()` stays
            // absent) keep that; if the inner REJECTS undefined, the key is
            // simply absent — the wrapper still accepts it.
            if input.is_none() {
                let mut inner_issues = Vec::new();
                match parse_at(inner, None, path, &mut inner_issues) {
                    Ok(some) => Ok(some),
                    // Inner rejected undefined → the key is absent, not an error.
                    Err(()) => Ok(None),
                }
            } else {
                parse_at(inner, input, path, issues)
            }
        }
        Schema::Nullable(inner) => {
            match input {
                // explicit null → null
                Some(v) if v.is_null() => Ok(Some(Value::Null)),
                // undefined → dropped (matches the optional chain it rides on;
                // oracle: `z.string().optional().nullable()` absent succeeds)
                None => Ok(None),
                Some(_) => parse_at(inner, input, path, issues),
            }
        }
        Schema::Default(inner, fill) => {
            if input.is_none() {
                Ok(Some(fill.clone()))
            } else {
                parse_at(inner, input, path, issues)
            }
        }
        Schema::Refine(f, message, inner) => {
            match parse_at(inner, input, path, issues) {
                Ok(None) => Ok(None),
                Ok(Some(data)) => {
                    if f(&data) {
                        Ok(Some(data))
                    } else {
                        issues.push(custom_issue(path, message));
                        Err(())
                    }
                }
                Err(()) => {
                    // zod's refinements are checks, and checks are
                    // non-aborting among themselves: a failed string CHECK
                    // (min/regex/an earlier refine) still runs this refine on
                    // the typed value, while a TYPE failure — wrong input
                    // type, or a failed object property — skips it (oracle:
                    // refine_chain_both_fail / min_then_refine_fail vs
                    // type_fail_skips_refine / object_field_fail_skips_refine).
                    if string_type_stage_passed(inner, input) && !f(input.unwrap()) {
                        issues.push(custom_issue(path, message));
                    }
                    Err(())
                }
            }
        }
        // Same position as `.refine`, but the check supplies the message.
        Schema::SuperRefine(f, inner) => {
            let data = parse_at(inner, input, path, issues)?;
            match data {
                None => Ok(None),
                Some(data) => match f(&data) {
                    None => Ok(Some(data)),
                    Some(added) => {
                        let mut issue = custom_issue(path, &added.message);
                        issue.params = added.params;
                        issues.push(issue);
                        Err(())
                    }
                },
            }
        }
        Schema::Transform(f, inner) => {
            let data = parse_at(inner, input, path, issues)?;
            Ok(data.map(f))
        }
        // `.catch(undefined)`: a failing inner parse resolves to `undefined`
        // (the key vanishes); its issues never surface.
        Schema::CatchUndefined(inner) => {
            let mut swallowed = Vec::new();
            match parse_at(inner, input, path, &mut swallowed) {
                Ok(data) => Ok(data),
                Err(()) => Ok(None),
            }
        }
        // `.catch(fn → undefined)`: same resolution as `CatchUndefined`, but
        // the swallowed issues are handed to the callback first — zod invokes
        // the catch function only on a failing parse.
        Schema::CatchUndefinedWith(f, inner) => {
            let mut swallowed = Vec::new();
            match parse_at(inner, input, path, &mut swallowed) {
                Ok(data) => Ok(data),
                Err(()) => {
                    f(&ZodError::new(swallowed));
                    Ok(None)
                }
            }
        }
        // `.check(ctx)`: post-validation, the checker owns paths and messages;
        // every pushed issue is `custom` with the path appended to the site's.
        Schema::Check(f, inner) => {
            let data = parse_at(inner, input, path, issues)?;
            match data {
                None => Ok(None),
                Some(data) => {
                    let found = f(&data);
                    if found.is_empty() {
                        Ok(Some(data))
                    } else {
                        for check_issue in found {
                            let mut full_path = path.to_vec();
                            full_path.extend(check_issue.path);
                            issues.push(Issue {
                                code: IssueCode::Custom,
                                path: full_path,
                                message: check_issue.message,
                                ..Issue::default()
                            });
                        }
                        Err(())
                    }
                }
            }
        }
        // z.any()/z.unknown() preserve undefined; an optional object field
        // must remain absent rather than becoming an explicit null.
        Schema::Any => Ok(input.cloned()),
        Schema::String {
            check_order,
            min_length,
            min_message,
            length,
            starts_with,
            starts_with_message,
            ends_with,
            pattern_flags,
            pattern,
            pattern_message,
            format,
        } => {
            let Some(s) = input.and_then(Value::as_str) else {
                issues.push(type_error(path, "string", input));
                return Err(());
            };
            // zod's string checks are NON-aborting: every failing check
            // reports its own issue (oracle: gitsha_combo_fail yields both
            // `too_small` and `invalid_format`). Only the type check above
            // stops the run. Checks run in the order chained, which for CC's
            // usages includes url before startsWith in MCP OAuth URLs.
            let before = issues.len();
            for check in check_order {
                match check {
                    super::schema::StringCheck::Min => {
                        if let Some(n) = min_length
                            && (s.encode_utf16().count() as u64) < *n
                        {
                            let mut issue =
                                size_issue(path, IssueCode::TooSmall, "string", *n, "characters");
                            // `.min(n, message)` swaps the text only (oracle:
                            // string_min_custom_message).
                            if let Some(message) = min_message {
                                issue.message = (*message).to_string();
                            }
                            issues.push(issue);
                        }
                    }
                    super::schema::StringCheck::Length => {
                        if let Some(n) = length {
                            // `.length(n)` reports the failing side with `exact: true`
                            // (oracle: string_length_short / _long).
                            let chars = s.encode_utf16().count() as u64;
                            if chars < *n {
                                let mut issue = size_issue(
                                    path,
                                    IssueCode::TooSmall,
                                    "string",
                                    *n,
                                    "characters",
                                );
                                issue.exact = Some(true);
                                issues.push(issue);
                            } else if chars > *n {
                                let mut issue =
                                    size_issue(path, IssueCode::TooBig, "string", *n, "characters");
                                issue.exact = Some(true);
                                issues.push(issue);
                            }
                        }
                    }
                    super::schema::StringCheck::Prefix => {
                        if let Some(p) = starts_with
                            && !s.starts_with(p)
                        {
                            let mut issue = starts_with_issue(path, p);
                            if let Some(message) = starts_with_message {
                                issue.message = (*message).to_string();
                            }
                            issues.push(issue);
                        }
                    }
                    super::schema::StringCheck::Suffix => {
                        if let Some(suffix) = ends_with
                            && !s.ends_with(suffix)
                        {
                            issues.push(Issue {
                                code: IssueCode::InvalidFormat,
                                origin: Some("string"),
                                format: Some("ends_with"),
                                suffix: Some((*suffix).to_string()),
                                path: path.clone(),
                                message: format!("Invalid string: must end with \"{suffix}\""),
                                ..Issue::default()
                            });
                        }
                    }
                    super::schema::StringCheck::Regex => {
                        if let Some(p) = pattern {
                            let matches = if let Some(flags) = pattern_flags {
                                regex::bytes::RegexBuilder::new(p)
                                    .unicode(false)
                                    .case_insensitive(*flags == "i")
                                    .build()
                                    .is_ok_and(|re| re.is_match(s.as_bytes()))
                            } else {
                                regex::Regex::new(p).is_ok_and(|re| re.is_match(s))
                            };
                            if !matches {
                                let mut issue = regex_issue(path, p, pattern_flags.unwrap_or(""));
                                // `.regex(re, message)` swaps the text only.
                                if let Some(message) = pattern_message {
                                    issue.message = (*message).to_string();
                                }
                                issues.push(issue);
                            }
                        }
                    }
                    super::schema::StringCheck::Format => {
                        if let Some(f) = format
                            && !format_matches(*f, s)
                        {
                            issues.push(format_issue(path, *f));
                        }
                    }
                }
            }
            if issues.len() > before {
                return Err(());
            }
            Ok(Some(input.cloned().unwrap()))
        }
        Schema::Boolean => match input.and_then(Value::as_bool) {
            Some(_) => Ok(Some(input.cloned().unwrap())),
            None => {
                issues.push(type_error(path, "boolean", input));
                Err(())
            }
        },
        Schema::Number {
            int,
            positive,
            nonnegative,
            min,
            max,
        } => {
            let Some(n) = input.and_then(Value::as_f64) else {
                issues.push(type_error(path, "number", input));
                return Err(());
            };
            if *int {
                if n.fract() != 0.0 {
                    issues.push(int_fract_issue(path));
                    return Err(());
                }
                if n > MAX_SAFE_INTEGER {
                    issues.push(int_bound_issue(path, IssueCode::TooBig));
                    return Err(());
                }
                if n < -MAX_SAFE_INTEGER {
                    issues.push(int_bound_issue(path, IssueCode::TooSmall));
                    return Err(());
                }
            }
            if *positive && n <= 0.0 {
                issues.push(bound_issue(path, IssueCode::TooSmall, 0.0, false, ">0"));
                return Err(());
            }
            if *nonnegative && n < 0.0 {
                issues.push(bound_issue(path, IssueCode::TooSmall, 0.0, true, ">=0"));
                return Err(());
            }
            // `.min()` / `.max()` are inclusive (oracle: num_min / num_max).
            if let Some(bound) = min
                && n < *bound
            {
                let desc = format!(">={}", super::javascript_number_to_string(*bound));
                issues.push(bound_issue(path, IssueCode::TooSmall, *bound, true, &desc));
                return Err(());
            }
            if let Some(bound) = max
                && n > *bound
            {
                let desc = format!("<={}", super::javascript_number_to_string(*bound));
                issues.push(bound_issue(path, IssueCode::TooBig, *bound, true, &desc));
                return Err(());
            }
            Ok(Some(input.cloned().unwrap()))
        }
        Schema::Literal(expected) => {
            if input == Some(expected) {
                Ok(Some(input.cloned().unwrap()))
            } else {
                issues.push(literal_issue(path, expected));
                Err(())
            }
        }
        Schema::Enum(values) => match input.and_then(Value::as_str) {
            Some(s) if values.contains(&s) => Ok(Some(input.cloned().unwrap())),
            _ => {
                issues.push(enum_issue(path, values));
                Err(())
            }
        },
        Schema::Array {
            items: inner,
            min_items,
            min_message,
            max_items,
        } => {
            let Some(items) = input.and_then(Value::as_array) else {
                issues.push(type_error(path, "array", input));
                return Err(());
            };
            // The count checks run before the per-item ones and short-circuit.
            if let Some(n) = min_items
                && (items.len() as u64) < *n
            {
                let mut issue = size_issue(path, IssueCode::TooSmall, "array", *n, "items");
                // `.min(n, message)` swaps the text only.
                if let Some(message) = min_message {
                    issue.message = (*message).to_string();
                }
                issues.push(issue);
                return Err(());
            }
            if let Some(n) = max_items
                && (items.len() as u64) > *n
            {
                issues.push(size_issue(path, IssueCode::TooBig, "array", *n, "items"));
                return Err(());
            }
            let mut out = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                path.push(PathSegment::Index(i));
                match parse_at(inner, Some(item), path, issues) {
                    Ok(Some(v)) => out.push(v),
                    Ok(None) => out.push(Value::Null),
                    Err(()) => { /* issue recorded; keep collecting */ }
                }
                path.pop();
            }
            if issues.is_empty() {
                Ok(Some(Value::Array(out)))
            } else {
                Err(())
            }
        }
        Schema::Record(inner) => parse_record(None, inner, input, path, issues),
        Schema::RecordWithKey { key, value } => parse_record(Some(key), value, input, path, issues),
        Schema::PartialRecord { keys, value } => {
            let Some(map) = record_object(input) else {
                issues.push(type_error(path, "record", input));
                return Err(());
            };
            let before = issues.len();
            let mut out = serde_json::Map::new();
            for (k, v) in crate::utils::process_env::ecmascript_object_entries(map) {
                if k == "__proto__" {
                    continue;
                }
                if !keys.contains(&k) {
                    // A key outside the enum is `invalid_key`, nesting the
                    // enum-or-never union failure zod builds internally
                    // (oracle: partial_record_bad_key, structure verbatim).
                    let union_issue = Issue {
                        code: IssueCode::InvalidUnion,
                        message: "Invalid input".to_string(),
                        errors: vec![
                            vec![enum_issue(&[], keys)],
                            vec![Issue {
                                code: IssueCode::InvalidType,
                                expected: Some("never"),
                                message: "Invalid input: expected never, received string"
                                    .to_string(),
                                ..Issue::default()
                            }],
                        ],
                        ..Issue::default()
                    };
                    let mut key_path = path.clone();
                    key_path.push(PathSegment::Key(k.to_owned()));
                    issues.push(Issue {
                        code: IssueCode::InvalidKey,
                        origin: Some("record"),
                        path: key_path,
                        message: "Invalid key in record".to_string(),
                        issues: vec![union_issue],
                        ..Issue::default()
                    });
                    continue;
                }
                path.push(PathSegment::Key(k.to_owned()));
                if let Ok(Some(parsed)) = parse_at(value, Some(v), path, issues) {
                    out.insert(k.to_owned(), parsed);
                }
                path.pop();
            }
            if issues.len() > before {
                Err(())
            } else {
                Ok(Some(Value::Object(out)))
            }
        }
        Schema::StrictObject(shape) => {
            parse_object(shape, UnknownKeys::Strict, input, path, issues)
        }
        Schema::Object(shape) => parse_object(shape, UnknownKeys::Strip, input, path, issues),
        Schema::PassthroughObject(shape) => {
            parse_object(shape, UnknownKeys::Passthrough, input, path, issues)
        }
        Schema::Union(variants) => {
            let mut errors = Vec::new();
            for variant in variants {
                let mut variant_issues = Vec::new();
                if let Ok(data) = parse_at(variant, input, &mut Vec::new(), &mut variant_issues)
                    && variant_issues.is_empty()
                {
                    return Ok(data);
                }
                errors.push(variant_issues);
            }
            issues.push(Issue {
                code: IssueCode::InvalidUnion,
                path: path.to_vec(),
                message: "Invalid input".to_string(),
                errors,
                ..Issue::default()
            });
            Err(())
        }
        Schema::DiscriminatedUnion { tag, variants } => {
            let Some(map) = input.and_then(Value::as_object) else {
                issues.push(type_error(path, "object", input));
                return Err(());
            };
            let mut options: Vec<String> = Vec::new();
            for variant in variants {
                // zod reads the discriminator through `.describe()` — the
                // settings arm of MarketplaceSourceSchema is a described
                // object, and HookCommandSchema's tag fields are described
                // literals; both still discriminate.
                if let Some(shape) = variant_object_shape(variant)
                    && let Some(lit) = shape_get(shape, tag).and_then(tag_literal)
                    && let Some(lit_str) = lit.as_str()
                {
                    options.push(lit_str.to_string());
                    if map.get(*tag).and_then(Value::as_str) == Some(lit_str) {
                        return parse_at(variant, input, path, issues);
                    }
                }
            }
            let mut issue_path = path.to_vec();
            issue_path.push(PathSegment::Key((*tag).to_string()));
            issues.push(Issue {
                code: IssueCode::InvalidUnion,
                path: issue_path,
                message: "Invalid input".to_string(),
                note: Some("No matching discriminator"),
                ..Issue::default()
            });
            Err(())
        }
    }
}

/// What an object does with keys its shape does not declare — zod's three
/// object constructors, one per policy.
#[derive(Clone, Copy, PartialEq, Eq)]
enum UnknownKeys {
    /// `z.object({...})` — drop them from the parsed data, no issue.
    Strip,
    /// `z.strictObject({...})` — report `unrecognized_keys`.
    Strict,
    /// `z.object({...}).passthrough()` — carry them through untouched.
    Passthrough,
}

fn parse_object(
    shape: &super::Shape,
    unknown_keys: UnknownKeys,
    input: Option<&Value>,
    path: &mut Vec<PathSegment>,
    issues: &mut Vec<Issue>,
) -> Result<Option<Value>, ()> {
    let Some(map) = input.and_then(Value::as_object) else {
        issues.push(type_error(path, "object", input));
        return Err(());
    };

    let mut out = serde_json::Map::new();
    // Field errors first, in declaration order; `unrecognized_keys` last.
    for (key, field_schema) in shape.iter() {
        path.push(PathSegment::Key((*key).to_string()));
        // An absent key runs its field schema against `undefined` (zod/v4); the
        // field decides — `.optional()` drops, `.default()` fills, a wrapping
        // `.transform()` may produce a value, anything else is a missing
        // required parameter. `None` here is that undefined.
        let field_input = map.get(*key);
        match parse_at(field_schema, field_input, path, issues) {
            Ok(Some(v)) => {
                out.insert((*key).to_string(), v);
            }
            Ok(None) => { /* resolved to undefined → drop */ }
            Err(()) => { /* issue recorded (missing required or invalid) */ }
        }
        path.pop();
    }

    if unknown_keys != UnknownKeys::Strip {
        let stray: Vec<_> = crate::utils::process_env::ecmascript_object_entries(map)
            .into_iter()
            .filter(|(key, _)| shape_get(shape, key).is_none())
            .collect();
        match unknown_keys {
            UnknownKeys::Strict if !stray.is_empty() => {
                issues.push(unrecognized_issue(
                    path,
                    stray.into_iter().map(|(key, _)| key.to_owned()).collect(),
                ));
            }
            UnknownKeys::Passthrough => {
                for (key, value) in stray {
                    out.insert(key.to_owned(), value.clone());
                }
            }
            _ => {}
        }
    }

    if issues.is_empty() {
        Ok(Some(Value::Object(out)))
    } else {
        Err(())
    }
}

// ── Issue constructors, byte-aligned to the zod/v4 oracle ────────────────

fn type_error(path: &[PathSegment], expected: &'static str, input: Option<&Value>) -> Issue {
    let received = match input {
        None => "undefined",
        Some(v) => js_type_of(v),
    };
    Issue {
        code: IssueCode::InvalidType,
        path: path.to_vec(),
        message: format!("Invalid input: expected {expected}, received {received}"),
        expected: Some(expected),
        ..Issue::default()
    }
}

fn int_fract_issue(path: &[PathSegment]) -> Issue {
    Issue {
        code: IssueCode::InvalidType,
        path: path.to_vec(),
        message: "Invalid input: expected int, received number".to_string(),
        expected: Some("int"),
        format: Some("safeint"),
        ..Issue::default()
    }
}

fn int_bound_issue(path: &[PathSegment], code: IssueCode) -> Issue {
    let (bound, msg) = match code {
        IssueCode::TooBig => (
            MAX_SAFE_INTEGER,
            "Too big: expected int to be <9007199254740991".to_string(),
        ),
        _ => (
            -MAX_SAFE_INTEGER,
            "Too small: expected int to be >=-9007199254740991".to_string(),
        ),
    };
    let mut issue = Issue {
        code,
        path: path.to_vec(),
        message: msg,
        origin: Some("int"),
        inclusive: Some(true),
        note: Some("Integers must be within the safe integer range."),
        ..Issue::default()
    };
    match code {
        IssueCode::TooBig => issue.maximum = Some(bound),
        _ => issue.minimum = Some(bound),
    }
    issue
}

fn bound_issue(
    path: &[PathSegment],
    code: IssueCode,
    bound: f64,
    inclusive: bool,
    desc: &str,
) -> Issue {
    let word = if code == IssueCode::TooBig {
        "Too big"
    } else {
        "Too small"
    };
    let mut issue = Issue {
        code,
        path: path.to_vec(),
        message: format!("{word}: expected number to be {desc}"),
        origin: Some("number"),
        inclusive: Some(inclusive),
        ..Issue::default()
    };
    match code {
        IssueCode::TooBig => issue.maximum = Some(bound),
        _ => issue.minimum = Some(bound),
    }
    issue
}

fn literal_issue(path: &[PathSegment], expected: &Value) -> Issue {
    Issue {
        code: IssueCode::InvalidValue,
        path: path.to_vec(),
        message: format!("Invalid input: expected {}", expected),
        values: Some(vec![expected.clone()]),
        ..Issue::default()
    }
}

fn enum_issue(path: &[PathSegment], values: &[&'static str]) -> Issue {
    // zod/v4 uses the single expected-value message for a one-member enum.
    if let [value] = values {
        return literal_issue(path, &json_str(value));
    }
    let joined = values
        .iter()
        .map(|v| format!("\"{v}\""))
        .collect::<Vec<_>>()
        .join("|");
    Issue {
        code: IssueCode::InvalidValue,
        path: path.to_vec(),
        message: format!("Invalid option: expected one of {joined}"),
        values: Some(values.iter().map(|v| json_str(v)).collect()),
        ..Issue::default()
    }
}

fn unrecognized_issue(path: &[PathSegment], keys: Vec<String>) -> Issue {
    let quoted: Vec<String> = keys.iter().map(|k| format!("\"{k}\"")).collect();
    let message = if keys.len() == 1 {
        format!("Unrecognized key: {}", quoted[0])
    } else {
        format!("Unrecognized keys: {}", quoted.join(", "))
    };
    Issue {
        code: IssueCode::UnrecognizedKeys,
        path: path.to_vec(),
        message,
        keys,
        ..Issue::default()
    }
}

/// Maps to: zod/v4's `too_small` / `too_big` from a `.min()` / `.max()` size
/// check. `origin` names what was measured ("string" / "array") and `unit` the
/// noun in the sentence ("characters" / "items"); both are the oracle's, not a
/// reading of the docs.
fn size_issue(
    path: &[PathSegment],
    code: IssueCode,
    origin: &'static str,
    bound: u64,
    unit: &str,
) -> Issue {
    let (word, comparator) = match code {
        IssueCode::TooBig => ("Too big", "<="),
        _ => ("Too small", ">="),
    };
    let mut issue = Issue {
        code,
        path: path.to_vec(),
        message: format!("{word}: expected {origin} to have {comparator}{bound} {unit}"),
        origin: Some(origin),
        inclusive: Some(true),
        ..Issue::default()
    };
    match issue.code {
        IssueCode::TooBig => issue.maximum = Some(bound as f64),
        _ => issue.minimum = Some(bound as f64),
    }
    issue
}

/// Maps to: zod/v4/core/util.js:131-146 `isPlainObject`, for JSON values.
/// An absent constructor retains Object.prototype's constructor. An own
/// non-null constructor must expose a prototype with its own isPrototypeOf.
/// Partial: own constructor:null throws in JS while this carrier can only
/// return Zod issues. That exception is not represented here; do not invent
/// a Zod issue or hard-code the runtime's TypeError message for it.
fn record_object(input: Option<&Value>) -> Option<&serde_json::Map<String, Value>> {
    let map = input.and_then(Value::as_object)?;
    if let Some(constructor) = map.get("constructor").filter(|value| !value.is_null()) {
        let prototype = constructor.get("prototype").and_then(Value::as_object)?;
        if !prototype.contains_key("isPrototypeOf") {
            return None;
        }
    }
    Some(map)
}

/// Maps to: zod/v4 record parsing. Both unrestricted and validated string
/// keys share the value parse loop; key failure skips that value's validation.
fn parse_record(
    key: Option<&Schema>,
    value: &Schema,
    input: Option<&Value>,
    path: &mut Vec<PathSegment>,
    issues: &mut Vec<Issue>,
) -> Result<Option<Value>, ()> {
    let Some(map) = record_object(input) else {
        issues.push(type_error(path, "record", input));
        return Err(());
    };
    let before = issues.len();
    let mut out = serde_json::Map::new();
    for (name, input_value) in crate::utils::process_env::ecmascript_object_entries(map) {
        // zod/v4/core/schemas.js skips this own key before validating it.
        if name == "__proto__" {
            continue;
        }
        let mut key_issues = Vec::new();
        let parsed_key = match key {
            Some(key) => parse_at(
                key,
                Some(&Value::String(name.to_owned())),
                &mut Vec::new(),
                &mut key_issues,
            ),
            None => Ok(Some(Value::String(name.to_owned()))),
        };
        path.push(PathSegment::Key(name.to_owned()));
        if parsed_key.is_err() || !key_issues.is_empty() {
            issues.push(Issue {
                code: IssueCode::InvalidKey,
                origin: Some("record"),
                path: path.clone(),
                message: "Invalid key in record".to_string(),
                issues: key_issues,
                ..Issue::default()
            });
        } else if let Ok(Some(parsed)) = parse_at(value, Some(input_value), path, issues) {
            let output_key = parsed_key
                .ok()
                .flatten()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_else(|| name.to_owned());
            out.insert(output_key, parsed);
        }
        path.pop();
    }
    if issues.len() > before {
        Err(())
    } else {
        Ok(Some(Value::Object(out)))
    }
}

/// Maps to: zod/v4's `invalid_format` from `.regex(re)`. The issue's `pattern`
/// keeps the `/` delimiters and the message quotes it, unlike the JSON Schema
/// `pattern`, which carries the bare source.
fn regex_issue(path: &[PathSegment], pattern: &str, flags: &str) -> Issue {
    let delimited = format!("/{pattern}/{flags}");
    Issue {
        code: IssueCode::InvalidFormat,
        path: path.to_vec(),
        message: format!("Invalid string: must match pattern {delimited}"),
        origin: Some("string"),
        format: Some("regex"),
        pattern: Some(delimited),
        ..Issue::default()
    }
}

/// Whether a failed inner parse was a string whose TYPE stage passed — i.e.
/// the failure came from checks (min/length/regex/an earlier refine), not from
/// the value being a non-string. Only then does zod run later refinements on
/// the (unchanged) string value. Object/record failures mark the payload
/// aborted in zod, so they answer false here (oracle:
/// object_field_fail_skips_refine). Capped at CC's usage: every refine chain
/// CC builds sits on a string or an object.
fn string_type_stage_passed(schema: &Schema, input: Option<&Value>) -> bool {
    match schema {
        Schema::String { .. } => input.is_some_and(Value::is_string),
        Schema::Refine(_, _, inner)
        | Schema::Describe(inner, _)
        | Schema::Optional(inner)
        | Schema::Nullable(inner) => string_type_stage_passed(inner, input),
        _ => false,
    }
}

/// Maps to: zod/v4's `invalid_format` from `.startsWith(p)`. The issue carries
/// `prefix` (no `pattern`) and quotes the prefix in the message (oracle:
/// string_starts_with_fail).
fn starts_with_issue(path: &[PathSegment], prefix: &str) -> Issue {
    Issue {
        code: IssueCode::InvalidFormat,
        path: path.to_vec(),
        message: format!("Invalid string: must start with \"{prefix}\""),
        origin: Some("string"),
        format: Some("starts_with"),
        prefix: Some(prefix.to_string()),
        ..Issue::default()
    }
}

/// Maps to: zod/v4's `invalid_format` from a format check such as `.url()`.
/// Note the absence of `origin` — zod does not set it on this one.
fn format_issue(path: &[PathSegment], format: super::schema::StringFormat) -> Issue {
    Issue {
        code: IssueCode::InvalidFormat,
        path: path.to_vec(),
        message: format.issue_message().to_string(),
        format: Some(format.issue_format()),
        ..Issue::default()
    }
}

/// zod's `.url()` accepts what the WHATWG URL parser accepts.
fn format_matches(format: super::schema::StringFormat, value: &str) -> bool {
    match format {
        super::schema::StringFormat::Url => url::Url::parse(value).is_ok(),
    }
}

fn custom_issue(path: &[PathSegment], message: &str) -> Issue {
    Issue {
        code: IssueCode::Custom,
        path: path.to_vec(),
        message: message.to_string(),
        ..Issue::default()
    }
}

fn shape_get<'a>(shape: &'a super::Shape, key: &str) -> Option<&'a Schema> {
    shape.iter().find(|(k, _)| *k == key).map(|(_, s)| s)
}

/// The literal a tag FIELD resolves to, looking through `.describe()` — CC's
/// hook schemas write `z.literal('command').describe(...)` on the tag itself.
fn tag_literal(schema: &Schema) -> Option<&Value> {
    match schema {
        Schema::Literal(v) => Some(v),
        Schema::Describe(inner, _) => tag_literal(inner),
        _ => None,
    }
}

/// The object shape a discriminated-union variant discriminates on, looking
/// through the wrappers zod itself looks through (a described or refined
/// variant still carries its tag).
fn variant_object_shape(variant: &Schema) -> Option<&super::Shape> {
    match variant {
        Schema::StrictObject(shape) | Schema::Object(shape) | Schema::PassthroughObject(shape) => {
            Some(shape)
        }
        Schema::Describe(inner, _)
        | Schema::Refine(_, _, inner)
        | Schema::SuperRefine(_, inner) => variant_object_shape(inner),
        _ => None,
    }
}

/// Maps to: zod/v4's `received` token — the JS `typeof` bucket.
fn js_type_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn json_str(s: &str) -> Value {
    Value::String(s.to_string())
}

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

#[cfg(test)]
mod tests {
    use super::super::schema::*;
    use super::*;
    use serde_json::json;

    /// Oracle: FileEditTool's `replace_all` is `semanticBoolean(z.boolean()
    /// .default(false).optional())` — absent must FILL, not drop.
    /// (`z.object({r:…}).safeParse({})` → `data {"r": false}`.)
    #[test]
    fn optional_default_fills_on_absent() {
        let schema = object(vec![("r", boolean().default(json!(false)).optional())]);
        let parsed = safe_parse(&schema, &json!({})).unwrap();
        assert_eq!(parsed, json!({"r": false}));
    }

    /// Oracle: `z.string().optional().nullable()` absent → success, key dropped.
    #[test]
    fn optional_nullable_drops_on_absent() {
        let schema = object(vec![("a", string().optional().nullable())]);
        let parsed = safe_parse(&schema, &json!({})).unwrap();
        assert_eq!(parsed, json!({}));
    }

    /// Oracle: strict object with missing fields AND a stray key collects all —
    /// two `invalid_type` (declaration order) then `unrecognized_keys` LAST.
    #[test]
    fn strict_object_collects_all_issues_with_unrecognized_last() {
        let schema = strict_object(vec![("a", string()), ("b", number())]);
        let err = safe_parse(&schema, &json!({"c": 1})).unwrap_err();
        assert_eq!(err.issues.len(), 3);
        assert_eq!(err.issues[0].code, IssueCode::InvalidType);
        assert_eq!(err.issues[0].path, vec![PathSegment::Key("a".to_string())]);
        assert_eq!(err.issues[1].code, IssueCode::InvalidType);
        assert_eq!(err.issues[1].path, vec![PathSegment::Key("b".to_string())]);
        assert_eq!(err.issues[2].code, IssueCode::UnrecognizedKeys);
        assert_eq!(err.issues[2].keys, vec!["c".to_string()]);
    }

    /// Oracle: `zodError.message` is the whole issues tree as pretty JSON, and
    /// it must contain the verbatim per-issue messages.
    #[test]
    fn error_message_is_the_issues_dump() {
        let schema = enumeration(vec!["a", "b"]);
        let err = safe_parse(&schema, &json!("z")).unwrap_err();
        let message = err.message();
        assert!(message.contains("\"code\": \"invalid_value\""));
        assert!(message.contains("Invalid option: expected one of \\\"a\\\"|\\\"b\\\""));
        assert!(
            message.starts_with('['),
            "message is the issues array, got {message}"
        );
    }

    /// Oracle: `.int()` past the safe bound is `too_big` (`<=` bound), below is
    /// `too_small`, a fraction is `invalid_type`/`expected int`.
    #[test]
    fn int_bounds_and_fraction_codes_match_oracle() {
        let over = safe_parse(&number().int(), &json!(9007199254740992_f64)).unwrap_err();
        assert_eq!(over.issues[0].code, IssueCode::TooBig);
        assert!(over.issues[0].message.contains("<9007199254740991"));

        let under = safe_parse(&number().int(), &json!(-9007199254740992_f64)).unwrap_err();
        assert_eq!(under.issues[0].code, IssueCode::TooSmall);

        let fract = safe_parse(&number().int(), &json!(1.5)).unwrap_err();
        assert_eq!(fract.issues[0].code, IssueCode::InvalidType);
        assert_eq!(fract.issues[0].expected, Some("int"));
        assert_eq!(fract.issues[0].format, Some("safeint"));
    }

    /// `.catch(fn → undefined)` invokes the callback only on a FAILING parse,
    /// handing it the swallowed issues — success and absence never fire it
    /// (zod/v4 `.catch(fn)`; CC's use is the updatedPermissions warn log,
    /// `PermissionPromptToolResultSchema.ts:53-59`).
    #[test]
    fn catch_undefined_with_fires_only_on_failure() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static CATCH_CALLS: AtomicUsize = AtomicUsize::new(0);
        fn record_catch(error: &ZodError) {
            assert!(!error.issues.is_empty(), "callback receives the issues");
            CATCH_CALLS.fetch_add(1, Ordering::SeqCst);
        }
        let schema = object(vec![(
            "p",
            array(string())
                .optional()
                .catch_undefined_with(record_catch),
        )]);
        // Absent: `.optional()` resolves undefined — nothing failed.
        assert_eq!(safe_parse(&schema, &json!({})).unwrap(), json!({}));
        assert_eq!(CATCH_CALLS.load(Ordering::SeqCst), 0);
        // Valid present: parses — nothing failed.
        assert_eq!(
            safe_parse(&schema, &json!({"p": ["a"]})).unwrap(),
            json!({"p": ["a"]})
        );
        assert_eq!(CATCH_CALLS.load(Ordering::SeqCst), 0);
        // Wrong-typed present: swallowed to undefined (key vanishes, no
        // issues surface), and the callback observes the swallowed error.
        assert_eq!(safe_parse(&schema, &json!({"p": 7})).unwrap(), json!({}));
        assert_eq!(CATCH_CALLS.load(Ordering::SeqCst), 1);
    }

    /// Oracle: discriminated union on a non-object reports invalid_type object,
    /// not a discriminator error.
    #[test]
    fn discriminated_union_on_non_object_reports_object_type() {
        let schema = discriminated_union("type", vec![object(vec![("type", literal(json!("a")))])]);
        let err = safe_parse(&schema, &json!("hello")).unwrap_err();
        assert_eq!(err.issues[0].code, IssueCode::InvalidType);
        assert_eq!(err.issues[0].expected, Some("object"));
        assert!(err.issues[0].path.is_empty());
    }

    /// Oracle: union failure nests one issue list per variant, with the
    /// variant-internal paths RELATIVE (empty), outer path on the field.
    #[test]
    fn union_nests_relative_issue_paths() {
        let schema = object(vec![("u", union(vec![string(), number()]))]);
        let err = safe_parse(&schema, &json!({"u": true})).unwrap_err();
        let issue = &err.issues[0];
        assert_eq!(issue.code, IssueCode::InvalidUnion);
        assert_eq!(issue.path, vec![PathSegment::Key("u".to_string())]);
        assert_eq!(issue.errors.len(), 2);
        assert!(
            issue.errors[0][0].path.is_empty(),
            "variant path is relative"
        );
        assert_eq!(issue.errors[0][0].expected, Some("string"));
        assert_eq!(issue.errors[1][0].expected, Some("number"));
    }
}

#[cfg(test)]
mod plugin_validation_carrier_tests {
    use crate::utils::zod::*;
    use serde_json::json;

    // Actual Bun 1.3.14 + rebuild's zod/v4 oracle. Source and full run are kept
    // under research/proof/plugin-validate-zod-0914; assertions are inline here.
    #[test]
    fn plugin_schema_combinators_match_bun_data_issues_and_projection() {
        const ORACLE: &str = r###"[{"name":"ends_fail","input":"bad","result":{"success":false,"issues":[{"origin":"string","code":"invalid_format","format":"starts_with","prefix":"./","path":[],"message":"Invalid string: must start with \"./\""},{"origin":"string","code":"invalid_format","format":"ends_with","suffix":".json","path":[],"message":"Invalid string: must end with \".json\""}],"message":"[\n  {\n    \"origin\": \"string\",\n    \"code\": \"invalid_format\",\n    \"format\": \"starts_with\",\n    \"prefix\": \"./\",\n    \"path\": [],\n    \"message\": \"Invalid string: must start with \\\"./\\\"\"\n  },\n  {\n    \"origin\": \"string\",\n    \"code\": \"invalid_format\",\n    \"format\": \"ends_with\",\n    \"suffix\": \".json\",\n    \"path\": [],\n    \"message\": \"Invalid string: must end with \\\".json\\\"\"\n  }\n]"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string","allOf":[{"pattern":"^\\.\\/.*"},{"pattern":".*\\.json$"}]}},{"name":"ends_ok","input":"./x.json","result":{"success":true,"data":"./x.json"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string","allOf":[{"pattern":"^\\.\\/.*"},{"pattern":".*\\.json$"}]}},{"name":"prefix_custom","input":"bad","result":{"success":false,"issues":[{"origin":"string","code":"invalid_format","format":"starts_with","prefix":"./","path":[],"message":"Relative only"}],"message":"[\n  {\n    \"origin\": \"string\",\n    \"code\": \"invalid_format\",\n    \"format\": \"starts_with\",\n    \"prefix\": \"./\",\n    \"path\": [],\n    \"message\": \"Relative only\"\n  }\n]"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string","pattern":"^\\.\\/.*"}},{"name":"url_prefix_order","input":"bad","result":{"success":false,"issues":[{"code":"invalid_format","format":"url","path":[],"message":"Invalid URL"},{"origin":"string","code":"invalid_format","format":"starts_with","prefix":"https://","path":[],"message":"HTTPS required"}],"message":"[\n  {\n    \"code\": \"invalid_format\",\n    \"format\": \"url\",\n    \"path\": [],\n    \"message\": \"Invalid URL\"\n  },\n  {\n    \"origin\": \"string\",\n    \"code\": \"invalid_format\",\n    \"format\": \"starts_with\",\n    \"prefix\": \"https://\",\n    \"path\": [],\n    \"message\": \"HTTPS required\"\n  }\n]"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string","format":"uri","pattern":"^https:\\/\\/.*"}},{"name":"suffix_prefix_order","input":"bad","result":{"success":false,"issues":[{"origin":"string","code":"invalid_format","format":"ends_with","suffix":".json","path":[],"message":"Invalid string: must end with \".json\""},{"origin":"string","code":"invalid_format","format":"starts_with","prefix":"./","path":[],"message":"Invalid string: must start with \"./\""}],"message":"[\n  {\n    \"origin\": \"string\",\n    \"code\": \"invalid_format\",\n    \"format\": \"ends_with\",\n    \"suffix\": \".json\",\n    \"path\": [],\n    \"message\": \"Invalid string: must end with \\\".json\\\"\"\n  },\n  {\n    \"origin\": \"string\",\n    \"code\": \"invalid_format\",\n    \"format\": \"starts_with\",\n    \"prefix\": \"./\",\n    \"path\": [],\n    \"message\": \"Invalid string: must start with \\\"./\\\"\"\n  }\n]"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string","allOf":[{"pattern":".*\\.json$"},{"pattern":"^\\.\\/.*"}]}},{"name":"anchor_0","input":"abc\n","result":{"success":false,"issues":[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*$/i","path":[],"message":"Invalid string: must match pattern /^[a-z0-9][-a-z0-9._]*$/i"}],"message":"[\n  {\n    \"origin\": \"string\",\n    \"code\": \"invalid_format\",\n    \"format\": \"regex\",\n    \"pattern\": \"/^[a-z0-9][-a-z0-9._]*$/i\",\n    \"path\": [],\n    \"message\": \"Invalid string: must match pattern /^[a-z0-9][-a-z0-9._]*$/i\"\n  }\n]"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string","pattern":"^[a-z0-9][-a-z0-9._]*$"}},{"name":"dependency_tail_0","input":"abc@^\n","result":{"success":true,"data":"abc@^\n"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string","pattern":"^[a-z0-9][-a-z0-9._]*(@[a-z0-9][-a-z0-9._]*)?(@\\^[^@]*)?$"}},{"name":"anchor_1","input":"abc\r","result":{"success":false,"issues":[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*$/i","path":[],"message":"Invalid string: must match pattern /^[a-z0-9][-a-z0-9._]*$/i"}],"message":"[\n  {\n    \"origin\": \"string\",\n    \"code\": \"invalid_format\",\n    \"format\": \"regex\",\n    \"pattern\": \"/^[a-z0-9][-a-z0-9._]*$/i\",\n    \"path\": [],\n    \"message\": \"Invalid string: must match pattern /^[a-z0-9][-a-z0-9._]*$/i\"\n  }\n]"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string","pattern":"^[a-z0-9][-a-z0-9._]*$"}},{"name":"dependency_tail_1","input":"abc@^\r","result":{"success":true,"data":"abc@^\r"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string","pattern":"^[a-z0-9][-a-z0-9._]*(@[a-z0-9][-a-z0-9._]*)?(@\\^[^@]*)?$"}},{"name":"anchor_2","input":"abc\u2028","result":{"success":false,"issues":[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*$/i","path":[],"message":"Invalid string: must match pattern /^[a-z0-9][-a-z0-9._]*$/i"}],"message":"[\n  {\n    \"origin\": \"string\",\n    \"code\": \"invalid_format\",\n    \"format\": \"regex\",\n    \"pattern\": \"/^[a-z0-9][-a-z0-9._]*$/i\",\n    \"path\": [],\n    \"message\": \"Invalid string: must match pattern /^[a-z0-9][-a-z0-9._]*$/i\"\n  }\n]"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string","pattern":"^[a-z0-9][-a-z0-9._]*$"}},{"name":"dependency_tail_2","input":"abc@^\u2028","result":{"success":true,"data":"abc@^\u2028"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string","pattern":"^[a-z0-9][-a-z0-9._]*(@[a-z0-9][-a-z0-9._]*)?(@\\^[^@]*)?$"}},{"name":"anchor_3","input":"abc\u2029","result":{"success":false,"issues":[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*$/i","path":[],"message":"Invalid string: must match pattern /^[a-z0-9][-a-z0-9._]*$/i"}],"message":"[\n  {\n    \"origin\": \"string\",\n    \"code\": \"invalid_format\",\n    \"format\": \"regex\",\n    \"pattern\": \"/^[a-z0-9][-a-z0-9._]*$/i\",\n    \"path\": [],\n    \"message\": \"Invalid string: must match pattern /^[a-z0-9][-a-z0-9._]*$/i\"\n  }\n]"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string","pattern":"^[a-z0-9][-a-z0-9._]*$"}},{"name":"dependency_tail_3","input":"abc@^\u2029","result":{"success":true,"data":"abc@^\u2029"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string","pattern":"^[a-z0-9][-a-z0-9._]*(@[a-z0-9][-a-z0-9._]*)?(@\\^[^@]*)?$"}},{"name":"record_bad_key","input":{"1bad":3,"valid":4},"result":{"success":false,"issues":[{"origin":"record","code":"invalid_key","issues":[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[A-Za-z_]\\w*$/","path":[],"message":"identifier"}],"path":["1bad"],"message":"Invalid key in record"},{"expected":"string","code":"invalid_type","path":["valid"],"message":"Invalid input: expected string, received number"}],"message":"[\n  {\n    \"origin\": \"record\",\n    \"code\": \"invalid_key\",\n    \"issues\": [\n      {\n        \"origin\": \"string\",\n        \"code\": \"invalid_format\",\n        \"format\": \"regex\",\n        \"pattern\": \"/^[A-Za-z_]\\\\w*$/\",\n        \"path\": [],\n        \"message\": \"identifier\"\n      }\n    ],\n    \"path\": [\n      \"1bad\"\n    ],\n    \"message\": \"Invalid key in record\"\n  },\n  {\n    \"expected\": \"string\",\n    \"code\": \"invalid_type\",\n    \"path\": [\n      \"valid\"\n    ],\n    \"message\": \"Invalid input: expected string, received number\"\n  }\n]"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","propertyNames":{"type":"string","pattern":"^[A-Za-z_]\\w*$"},"additionalProperties":{"type":"string"}}},{"name":"record_key_unicode","input":{"a\u4e2d":"v"},"result":{"success":false,"issues":[{"origin":"record","code":"invalid_key","issues":[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[A-Za-z_]\\w*$/","path":[],"message":"Invalid string: must match pattern /^[A-Za-z_]\\w*$/"}],"path":["a\u4e2d"],"message":"Invalid key in record"}],"message":"[\n  {\n    \"origin\": \"record\",\n    \"code\": \"invalid_key\",\n    \"issues\": [\n      {\n        \"origin\": \"string\",\n        \"code\": \"invalid_format\",\n        \"format\": \"regex\",\n        \"pattern\": \"/^[A-Za-z_]\\\\w*$/\",\n        \"path\": [],\n        \"message\": \"Invalid string: must match pattern /^[A-Za-z_]\\\\w*$/\"\n      }\n    ],\n    \"path\": [\n      \"a\u4e2d\"\n    ],\n    \"message\": \"Invalid key in record\"\n  }\n]"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","propertyNames":{"type":"string","pattern":"^[A-Za-z_]\\w*$"},"additionalProperties":{"type":"string"}}},{"name":"record_refine_key","input":{"bad":"v"},"result":{"success":false,"issues":[{"origin":"record","code":"invalid_key","issues":[{"code":"custom","path":[],"message":"dot"}],"path":["bad"],"message":"Invalid key in record"}],"message":"[\n  {\n    \"origin\": \"record\",\n    \"code\": \"invalid_key\",\n    \"issues\": [\n      {\n        \"code\": \"custom\",\n        \"path\": [],\n        \"message\": \"dot\"\n      }\n    ],\n    \"path\": [\n      \"bad\"\n    ],\n    \"message\": \"Invalid key in record\"\n  }\n]"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","propertyNames":{"type":"string"},"additionalProperties":{"type":"string"}}},{"name":"partial_default","input":{},"result":{"success":true,"data":{"a":"a"}},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"a":{"default":"a","type":"string"},"b":{"type":"string"}},"additionalProperties":false}},{"name":"extend_order","input":{"a":"wrong","b":2,"c":3,"x":1},"result":{"success":false,"issues":[{"expected":"number","code":"invalid_type","path":["a"],"message":"Invalid input: expected number, received string"},{"expected":"string","code":"invalid_type","path":["b"],"message":"Invalid input: expected string, received number"},{"expected":"string","code":"invalid_type","path":["c"],"message":"Invalid input: expected string, received number"},{"code":"unrecognized_keys","keys":["x"],"path":[],"message":"Unrecognized key: \"x\""}],"message":"[\n  {\n    \"expected\": \"number\",\n    \"code\": \"invalid_type\",\n    \"path\": [\n      \"a\"\n    ],\n    \"message\": \"Invalid input: expected number, received string\"\n  },\n  {\n    \"expected\": \"string\",\n    \"code\": \"invalid_type\",\n    \"path\": [\n      \"b\"\n    ],\n    \"message\": \"Invalid input: expected string, received number\"\n  },\n  {\n    \"expected\": \"string\",\n    \"code\": \"invalid_type\",\n    \"path\": [\n      \"c\"\n    ],\n    \"message\": \"Invalid input: expected string, received number\"\n  },\n  {\n    \"code\": \"unrecognized_keys\",\n    \"keys\": [\n      \"x\"\n    ],\n    \"path\": [],\n    \"message\": \"Unrecognized key: \\\"x\\\"\"\n  }\n]"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"a":{"type":"number"},"b":{"type":"string"},"c":{"type":"string"}},"required":["a","b","c"],"additionalProperties":false}},{"name":"strict_nested","input":{"a":{"b":"v","x":1}},"result":{"success":true,"data":{"a":{"b":"v"}}},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"a":{"type":"object","properties":{"b":{"type":"string"}},"required":["b"],"additionalProperties":false}},"required":["a"],"additionalProperties":false}},{"name":"regex_ABC","input":"ABC","result":{"success":true,"data":"ABC"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string","pattern":"^[a-z0-9][-a-z0-9._]*$"}},{"name":"regex_\u212a","input":"\u212a","result":{"success":false,"issues":[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*$/i","path":[],"message":"Invalid string: must match pattern /^[a-z0-9][-a-z0-9._]*$/i"}],"message":"[\n  {\n    \"origin\": \"string\",\n    \"code\": \"invalid_format\",\n    \"format\": \"regex\",\n    \"pattern\": \"/^[a-z0-9][-a-z0-9._]*$/i\",\n    \"path\": [],\n    \"message\": \"Invalid string: must match pattern /^[a-z0-9][-a-z0-9._]*$/i\"\n  }\n]"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string","pattern":"^[a-z0-9][-a-z0-9._]*$"}},{"name":"regex_\u017f","input":"\u017f","result":{"success":false,"issues":[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*$/i","path":[],"message":"Invalid string: must match pattern /^[a-z0-9][-a-z0-9._]*$/i"}],"message":"[\n  {\n    \"origin\": \"string\",\n    \"code\": \"invalid_format\",\n    \"format\": \"regex\",\n    \"pattern\": \"/^[a-z0-9][-a-z0-9._]*$/i\",\n    \"path\": [],\n    \"message\": \"Invalid string: must match pattern /^[a-z0-9][-a-z0-9._]*$/i\"\n  }\n]"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string","pattern":"^[a-z0-9][-a-z0-9._]*$"}},{"name":"regex_abc\n","input":"abc\n","result":{"success":false,"issues":[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*$/i","path":[],"message":"Invalid string: must match pattern /^[a-z0-9][-a-z0-9._]*$/i"}],"message":"[\n  {\n    \"origin\": \"string\",\n    \"code\": \"invalid_format\",\n    \"format\": \"regex\",\n    \"pattern\": \"/^[a-z0-9][-a-z0-9._]*$/i\",\n    \"path\": [],\n    \"message\": \"Invalid string: must match pattern /^[a-z0-9][-a-z0-9._]*$/i\"\n  }\n]"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string","pattern":"^[a-z0-9][-a-z0-9._]*$"}},{"name":"regex_a\u4e2d","input":"a\u4e2d","result":{"success":false,"issues":[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*$/i","path":[],"message":"Invalid string: must match pattern /^[a-z0-9][-a-z0-9._]*$/i"}],"message":"[\n  {\n    \"origin\": \"string\",\n    \"code\": \"invalid_format\",\n    \"format\": \"regex\",\n    \"pattern\": \"/^[a-z0-9][-a-z0-9._]*$/i\",\n    \"path\": [],\n    \"message\": \"Invalid string: must match pattern /^[a-z0-9][-a-z0-9._]*$/i\"\n  }\n]"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string","pattern":"^[a-z0-9][-a-z0-9._]*$"}},{"name":"source_id_ABC@m","input":"ABC@m","result":{"success":true,"data":"ABC@m"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string","pattern":"^[a-z0-9][-a-z0-9._]*@[a-z0-9][-a-z0-9._]*$"}},{"name":"source_id_\u212a@m","input":"\u212a@m","result":{"success":false,"issues":[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*@[a-z0-9][-a-z0-9._]*$/i","path":[],"message":"Plugin ID must be in format: plugin@marketplace"}],"message":"[\n  {\n    \"origin\": \"string\",\n    \"code\": \"invalid_format\",\n    \"format\": \"regex\",\n    \"pattern\": \"/^[a-z0-9][-a-z0-9._]*@[a-z0-9][-a-z0-9._]*$/i\",\n    \"path\": [],\n    \"message\": \"Plugin ID must be in format: plugin@marketplace\"\n  }\n]"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string","pattern":"^[a-z0-9][-a-z0-9._]*@[a-z0-9][-a-z0-9._]*$"}},{"name":"source_id_\u017f@m","input":"\u017f@m","result":{"success":false,"issues":[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*@[a-z0-9][-a-z0-9._]*$/i","path":[],"message":"Plugin ID must be in format: plugin@marketplace"}],"message":"[\n  {\n    \"origin\": \"string\",\n    \"code\": \"invalid_format\",\n    \"format\": \"regex\",\n    \"pattern\": \"/^[a-z0-9][-a-z0-9._]*@[a-z0-9][-a-z0-9._]*$/i\",\n    \"path\": [],\n    \"message\": \"Plugin ID must be in format: plugin@marketplace\"\n  }\n]"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string","pattern":"^[a-z0-9][-a-z0-9._]*@[a-z0-9][-a-z0-9._]*$"}},{"name":"source_id_abc@m\n","input":"abc@m\n","result":{"success":false,"issues":[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*@[a-z0-9][-a-z0-9._]*$/i","path":[],"message":"Plugin ID must be in format: plugin@marketplace"}],"message":"[\n  {\n    \"origin\": \"string\",\n    \"code\": \"invalid_format\",\n    \"format\": \"regex\",\n    \"pattern\": \"/^[a-z0-9][-a-z0-9._]*@[a-z0-9][-a-z0-9._]*$/i\",\n    \"path\": [],\n    \"message\": \"Plugin ID must be in format: plugin@marketplace\"\n  }\n]"},"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string","pattern":"^[a-z0-9][-a-z0-9._]*@[a-z0-9][-a-z0-9._]*$"}}]"###;
        for case in serde_json::from_str::<Vec<Value>>(ORACLE).unwrap() {
            let name = case["name"].as_str().unwrap();
            let schema = match name {
                "ends_fail" | "ends_ok" => string().starts_with("./").ends_with(".json"),
                "prefix_custom" => string().starts_with_message("./", "Relative only"),
                "url_prefix_order" => string()
                    .url()
                    .starts_with_message("https://", "HTTPS required"),
                "suffix_prefix_order" => string().ends_with(".json").starts_with("./"),
                "record_bad_key" => record_with_key(
                    string().regex_with_flags_and_message(r"^[A-Za-z_]\w*$", "", "identifier"),
                    string(),
                ),
                "record_key_unicode" => {
                    record_with_key(string().regex_with_flags(r"^[A-Za-z_]\w*$", ""), string())
                }
                "record_refine_key" => record_with_key(
                    string().refine(|v| v.as_str().unwrap().starts_with('.'), "dot"),
                    string(),
                ),
                "partial_default" => object(vec![
                    ("a", string().default(json!("a")).optional()),
                    ("b", string().optional()),
                ]),
                "extend_order" => {
                    object(vec![("a", number()), ("b", string()), ("c", string())]).strict()
                }
                "strict_nested" => object(vec![("a", object(vec![("b", string())]))]).strict(),
                name if name.starts_with("source_id_") => string().regex_with_flags_and_message(
                    r"^[a-z0-9][-a-z0-9._]*@[a-z0-9][-a-z0-9._]*$",
                    "i",
                    "Plugin ID must be in format: plugin@marketplace",
                ),
                name if name.starts_with("dependency_tail_") => string().regex_with_flags(
                    r"^[a-z0-9][-a-z0-9._]*(@[a-z0-9][-a-z0-9._]*)?(@\^[^@]*)?$",
                    "i",
                ),
                name if name.starts_with("regex_") || name.starts_with("anchor_") => {
                    string().regex_with_flags(r"^[a-z0-9][-a-z0-9._]*$", "i")
                }
                _ => panic!("unmapped oracle: {name}"),
            };
            let result = match safe_parse(&schema, &case["input"]) {
                Ok(data) => json!({"success":true,"data":data}),
                Err(error) => {
                    json!({"success":false,"issues":error.issues.iter().map(Issue::to_json).collect::<Vec<_>>(),"message":error.message()})
                }
            };
            assert_eq!(result, case["result"], "parse {name}");
            assert_eq!(to_json_schema(&schema), case["schema"], "projection {name}");
        }
    }

    #[test]
    fn plugin_object_shape_composition_retains_source_order_and_defaults() {
        let metadata = object(vec![("name", string())]);
        let Schema::Object(mut fields) = metadata else {
            unreachable!()
        };
        fields.push(("x", string().default(json!("default")).optional()));
        let schema = object(fields);
        assert_eq!(
            safe_parse(&schema, &json!({"name":"demo"})).unwrap(),
            json!({"name":"demo","x":"default"})
        );
        assert_eq!(
            match &schema {
                Schema::Object(fields) => fields,
                _ => unreachable!(),
            }
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>(),
            ["name", "x"]
        );
    }
}

#[cfg(test)]
mod plugin_validation_regressions {
    //! Regression evidence from actual rebuild Bun + zod/v4, 2026-09-14.
    //! Maps to: CC plugins/schemas.ts LSP/manifest consumers and zod core parsing.

    use crate::utils::zod::*;
    use serde_json::json;

    // Generated from research/proof/plugin-validate-review-a-0914/oracle.ts.
    // Kept inline so tests do not depend on task artifacts.
    const ORACLE: &str = r###"[{"name":"min_utf16","input":"🌙","data":"🌙"},{"name":"length_utf16","input":"🌙","data":"🌙"},{"name":"optional_unknown_absent","input":{},"data":{}},{"name":"optional_unknown_null","input":{"value":null},"data":{"value":null}},{"name":"optional_unknown_default","input":{},"data":{"value":"fill"}},{"name":"record_order","input":{"2":0,"10":0,"a":0},"issues":[{"expected":"string","code":"invalid_type","path":["2"],"message":"Invalid input: expected string, received number"},{"expected":"string","code":"invalid_type","path":["10"],"message":"Invalid input: expected string, received number"},{"expected":"string","code":"invalid_type","path":["a"],"message":"Invalid input: expected string, received number"}]},{"name":"record_order_data","input":{"2":0,"10":0,"a":0},"data":{"2":0,"10":0,"a":0}},{"name":"strict_order","input":{"2":0,"10":0,"name":"p"},"issues":[{"code":"unrecognized_keys","keys":["2","10"],"path":[],"message":"Unrecognized keys: \"2\", \"10\""}]},{"name":"partial_record_order","input":{"2":0,"10":0},"issues":[{"origin":"record","code":"invalid_key","issues":[{"code":"invalid_union","errors":[[{"code":"invalid_value","values":["ok"],"path":[],"message":"Invalid input: expected \"ok\""}],[{"expected":"never","code":"invalid_type","path":[],"message":"Invalid input: expected never, received string"}]],"path":[],"message":"Invalid input"}],"path":["2"],"message":"Invalid key in record"},{"origin":"record","code":"invalid_key","issues":[{"code":"invalid_union","errors":[[{"code":"invalid_value","values":["ok"],"path":[],"message":"Invalid input: expected \"ok\""}],[{"expected":"never","code":"invalid_type","path":[],"message":"Invalid input: expected never, received string"}]],"path":[],"message":"Invalid input"}],"path":["10"],"message":"Invalid key in record"}]},{"name":"record_proto_skip","input":{"__proto__":0,"ok":"yes"},"data":{"ok":"yes"}},{"name":"partial_record_proto_skip","input":{"__proto__":0,"ok":"yes"},"data":{"ok":"yes"}},{"name":"record_constructor_0","input":{"constructor":2},"issues":[{"expected":"record","code":"invalid_type","path":[],"message":"Invalid input: expected record, received object"}]},{"name":"record_constructor_1","input":{"constructor":true},"issues":[{"expected":"record","code":"invalid_type","path":[],"message":"Invalid input: expected record, received object"}]},{"name":"record_constructor_2","input":{"constructor":"x"},"issues":[{"expected":"record","code":"invalid_type","path":[],"message":"Invalid input: expected record, received object"}]},{"name":"record_constructor_3","input":{"constructor":[]},"issues":[{"expected":"record","code":"invalid_type","path":[],"message":"Invalid input: expected record, received object"}]},{"name":"record_constructor_4","input":{"constructor":{}},"issues":[{"expected":"record","code":"invalid_type","path":[],"message":"Invalid input: expected record, received object"}]},{"name":"record_constructor_5","input":{"constructor":{"prototype":{}}},"issues":[{"expected":"record","code":"invalid_type","path":[],"message":"Invalid input: expected record, received object"}]},{"name":"record_constructor_6","input":{"constructor":{"prototype":{"isPrototypeOf":false}}},"data":{"constructor":{"prototype":{"isPrototypeOf":false}}}},{"name":"source_lsp_utf16","input":{"command":"c","extensionToLanguage":{"🌙":"x"}},"issues":[{"origin":"record","code":"invalid_key","issues":[{"code":"custom","path":[],"message":"File extensions must start with dot (e.g., \".ts\", not \"ts\")"}],"path":["extensionToLanguage","🌙"],"message":"Invalid key in record"}]},{"name":"source_lsp_absence","input":{"command":"c","extensionToLanguage":{".x":"x"}},"data":{"command":"c","extensionToLanguage":{".x":"x"},"transport":"stdio"}},{"name":"source_manifest_constructor","input":{"name":"p","settings":{"constructor":2}},"issues":[{"expected":"record","code":"invalid_type","path":["settings"],"message":"Invalid input: expected record, received object"}]},{"name":"source_manifest_proto","input":{"name":"p","settings":{"__proto__":1}},"data":{"name":"p","settings":{}}}]"###;

    #[test]
    fn plugin_review_unicode_absence_and_record_boundaries_match_official_bun() {
        let cases: Vec<Value> = serde_json::from_str(ORACLE).unwrap();
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let schema = match name {
                "min_utf16" => string().min(2),
                "length_utf16" => string().length(2),
                "optional_unknown_absent" | "optional_unknown_null" => {
                    object(vec![("value", any().optional())])
                }
                "optional_unknown_default" => {
                    object(vec![("value", any().default(json!("fill")).optional())])
                }
                "record_order" | "record_proto_skip" => record(string()),
                "record_order_data" => record(any()),
                "strict_order" => strict_object(vec![("name", string())]),
                "partial_record_order" | "partial_record_proto_skip" => {
                    partial_record(vec!["ok"], string())
                }
                "source_lsp_utf16" | "source_lsp_absence" => {
                    crate::utils::plugins::schemas::lsp_server_config_schema().clone()
                }
                "source_manifest_constructor" | "source_manifest_proto" => {
                    crate::utils::plugins::schemas::plugin_manifest_schema()
                        .clone()
                        .strict()
                }
                name if name.starts_with("record_constructor_") => record(any()),
                _ => panic!("unmapped actual oracle {name}"),
            };
            // JSON.stringify canonicalizes the captured numeric keys. Recreate
            // their original insertion order so removing the shared own-key
            // projection really fails the ordering regression.
            let input = match name {
                "record_order" | "record_order_data" => {
                    serde_json::from_str(r#"{"10":0,"2":0,"a":0}"#).unwrap()
                }
                "strict_order" => serde_json::from_str(r#"{"name":"p","10":0,"2":0}"#).unwrap(),
                "partial_record_order" => serde_json::from_str(r#"{"10":0,"2":0}"#).unwrap(),
                _ => case["input"].clone(),
            };
            match safe_parse(&schema, &input) {
                Ok(data) => {
                    assert!(
                        case.get("data").is_some(),
                        "expected failure {name}: {data}"
                    );
                    assert_eq!(data, case["data"], "data {name}");
                    if let (Some(actual), Some(expected)) =
                        (data.as_object(), case["data"].as_object())
                    {
                        assert_eq!(
                            actual.keys().collect::<Vec<_>>(),
                            expected.keys().collect::<Vec<_>>(),
                            "own-key order {name}"
                        );
                    }
                }
                Err(error) => {
                    assert!(
                        case.get("issues").is_some(),
                        "unexpected failure {name}: {error:?}"
                    );
                    assert_eq!(
                        Value::Array(error.issues.iter().map(Issue::to_json).collect()),
                        case["issues"],
                        "complete ordered issue tree {name}"
                    );
                }
            }
        }
    }
}
