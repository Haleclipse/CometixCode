//! The schema value tree and its combinators.
//!
//! Each constructor / method maps to one zod/v4 API and carries the note.
//! `Schema` is a value: built once per tool (behind `LazyLock`, the
//! `lazySchema` counterpart), then consumed by `safe_parse` and
//! `to_json_schema`.

use super::Shape;

/// A field of an object schema: its name and its (possibly wrapped) schema.
pub type ObjectField = (&'static str, Schema);

/// A zod schema as a value tree.
///
/// Maps to: zod/v4 `ZodType`. The wrapper variants (`Optional`, `Default`,
/// `Preprocess`, `Describe`) wrap an inner schema exactly as zod's chainable
/// methods wrap the receiver.
#[derive(Clone, Debug)]
pub enum Schema {
    /// Maps to: zod/v4 `z.string()` and its checks. `min_length` is `.min(n)`,
    /// `pattern` is `.regex(re)` (stored without delimiters, as JSON Schema
    /// wants them; the issue message adds them back), and `format` is set by
    /// `.url()` — CC uses no other string format.
    String {
        check_order: Vec<StringCheck>,
        min_length: Option<u64>,
        /// `.min(n, message)`'s second argument — it replaces the generated
        /// issue text and nothing else (`TodoItemSchema`'s "Content cannot be
        /// empty"). The projection is unaffected.
        min_message: Option<&'static str>,
        /// `.length(n)` — an exact-length check (`gitSha`'s `.length(40)`).
        /// Projects `minLength` AND `maxLength`; a failing parse yields
        /// `too_small` / `too_big` with `exact: true` (oracle:
        /// string_length_short / _long).
        length: Option<u64>,
        /// `.startsWith(p)` (`RelativePath`'s `./`). Projects an anchored
        /// escaped-prefix `pattern`; a failing parse yields `invalid_format`
        /// with `format: "starts_with"` (oracle: string_starts_with_fail).
        starts_with: Option<&'static str>,
        starts_with_message: Option<&'static str>,
        ends_with: Option<&'static str>,
        /// Explicit JavaScript RegExp flags for the ASCII-pattern subset used by plugins.
        pattern_flags: Option<&'static str>,
        pattern: Option<&'static str>,
        /// `.regex(re, message)`'s second argument — replacement text for the
        /// generated `invalid_format` copy (`AllowedMcpServerEntrySchema`'s
        /// "Server name can only contain…"). The projection is unaffected.
        pattern_message: Option<&'static str>,
        format: Option<StringFormat>,
    },
    /// Maps to: zod/v4 `z.number()` — a JS double; `.int()` sets `int`, which
    /// carries zod's safe-integer bounds, NOT a hand-added `maximum`.
    Number {
        int: bool,
        positive: bool,
        nonnegative: bool,
        /// `.min(n)` / `.max(n)` — inclusive bounds, projected as `minimum` /
        /// `maximum` (unlike `.positive()`, which is `exclusiveMinimum`).
        min: Option<f64>,
        max: Option<f64>,
    },
    /// Maps to: zod/v4 `z.boolean()`.
    Boolean,
    /// Maps to: zod/v4 `z.literal(v)` → JSON Schema `{ "const": v }`.
    Literal(serde_json::Value),
    /// Maps to: zod/v4 `z.enum([...])` (string-valued).
    Enum(Vec<&'static str>),
    /// Maps to: zod/v4 `z.array(T)` with `.min(n)` / `.max(n)` item counts.
    Array {
        items: Box<Schema>,
        min_items: Option<u64>,
        /// `.min(n, message)` — the two-argument form's replacement text.
        min_message: Option<&'static str>,
        max_items: Option<u64>,
    },
    /// Maps to: zod/v4 `z.record(z.string(), T)` → `HashMap`-shaped object.
    Record(Box<Schema>),
    /// Maps to: zod/v4 `z.record(keySchema, valueSchema)` with validated string keys.
    RecordWithKey {
        key: Box<Schema>,
        value: Box<Schema>,
    },
    /// Maps to: zod/v4 `z.partialRecord(z.enum([...]), T)` — a record whose
    /// keys are limited to an enum and all optional (`HooksSchema`). A key
    /// outside the enum is an `invalid_key` issue; the projection names the
    /// keys via `propertyNames` (oracle: partial_record_*).
    PartialRecord {
        keys: Vec<&'static str>,
        value: Box<Schema>,
    },
    /// Maps to: zod/v4 `z.any()` / `z.unknown()` — accepts anything.
    Any,
    /// Maps to: zod/v4 `z.strictObject({...})` → `additionalProperties: false`
    /// and `unrecognized_keys` on stray input.
    StrictObject(Shape),
    /// Maps to: zod/v4 `z.object({...})` → strips unknown keys from the data.
    Object(Shape),
    /// Maps to: zod/v4 `z.object({...}).passthrough()` → keeps unknown keys in
    /// the data and projects `additionalProperties: {}` (an empty schema, which
    /// admits anything) rather than `false`.
    PassthroughObject(Shape),
    /// Maps to: zod/v4 `z.union([...])`.
    Union(Vec<Schema>),
    /// Maps to: zod/v4 `z.discriminatedUnion(tag, [...])` → the
    /// `{ "type": ..., "file": ... }` adjacently-tagged shape, and the
    /// discriminated-union JSON Schema, not a plain `oneOf`.
    DiscriminatedUnion {
        tag: &'static str,
        variants: Vec<Schema>,
    },
    /// Maps to: zod/v4 `.optional()` — `undefined`, distinct from `null`.
    Optional(Box<Schema>),
    /// Maps to: zod/v4 `.nullable()` — accepts explicit `null`, distinct from
    /// `undefined`. Projection is `anyOf: [inner, {"type":"null"}]`.
    Nullable(Box<Schema>),
    /// Maps to: zod/v4 `.default(v)` — fills the data when the key is absent.
    Default(Box<Schema>, serde_json::Value),
    /// Maps to: zod/v4 `z.preprocess(f, inner)`. Transform runs before
    /// validation; `to_json_schema` passes straight through to `inner`, so the
    /// tolerance is invisible to the model (`semanticBoolean.ts`).
    Preprocess(PreprocessFn, Box<Schema>),
    /// Maps to: zod/v4 `.refine(f, {message})`. Runs AFTER the inner schema
    /// validates; failure is a `custom` issue. Projection passes through to
    /// `inner`.
    Refine(RefineFn, &'static str, Box<Schema>),
    /// Maps to: zod/v4 `.superRefine((v, ctx) => ...)`. Same position as
    /// `.refine`, but the check owns the message — CC's one use
    /// (`EnterWorktreeTool.ts:27`) reports the thrown validator's own text, so
    /// a static string cannot stand in for it.
    SuperRefine(SuperRefineFn, Box<Schema>),
    /// Maps to: zod/v4 `.transform(f)`. Transforms the data after validation.
    /// zod/v4 throws on `toJSONSchema` ("Transforms cannot be represented in
    /// JSON Schema"); `to_json_schema` mirrors that by returning an error
    /// marker rather than a schema.
    Transform(TransformFn, Box<Schema>),
    /// Maps to: zod/v4 `.catch(undefined)` — a failing parse resolves to
    /// `undefined` (the key vanishes) instead of reporting issues. CC's two
    /// uses are both `.catch(undefined)` (`settings/types.ts:540,710`), so the
    /// undefined form is the only one carried. Projection passes through to
    /// `inner` (the tolerance is invisible to the schema consumer).
    CatchUndefined(Box<Schema>),
    /// Maps to: zod/v4 `.catch(ctx => { sideEffect(ctx.error); return
    /// undefined })` — the callback form of `.catch` whose fallback is still
    /// `undefined`, but which observes the swallowed error first. CC's one use
    /// logs the first issue (`PermissionPromptToolResultSchema.ts:53-59`);
    /// zod invokes the catch function only on a FAILING parse. Projection
    /// passes through to `inner`, exactly like `CatchUndefined`.
    CatchUndefinedWith(CatchUndefinedFn, Box<Schema>),
    /// Maps to: zod/v4 `.check(ctx => ...)` — a post-validation checker that
    /// can push MULTIPLE `custom` issues with their own paths and dynamic
    /// messages (`ctx.issues.push({code:'custom', path, message})`,
    /// `settings/types.ts:571-596`). Projection passes through to `inner`.
    Check(CheckFn, Box<Schema>),
    /// Maps to: zod/v4 `.describe(text)` — text enters the JSON Schema.
    Describe(Box<Schema>, String),
}

/// The transform half of `z.preprocess`. Pure: value in, value out.
pub type PreprocessFn = fn(&serde_json::Value) -> serde_json::Value;

/// The predicate half of `.refine`. `false` rejects.
pub type RefineFn = fn(&serde_json::Value) -> bool;

/// The check half of `.superRefine`. `Some(issue)` is one `ctx.addIssue({
/// code: 'custom', ... })`; `None` passes.
pub type SuperRefineFn = fn(&serde_json::Value) -> Option<SuperRefineIssue>;

/// The issue object a `.superRefine` check adds. `params` mirrors the
/// caller-attached extra object (`PermissionRuleSchema`'s `{received: val}`),
/// which survives into the issue dump (oracle: super_refine_params).
#[derive(Clone, Debug, PartialEq)]
pub struct SuperRefineIssue {
    pub message: String,
    pub params: Option<serde_json::Value>,
}

/// One issue a `.check(ctx)` checker pushes: a path relative to the checked
/// value plus the checker's own (usually formatted) message.
#[derive(Clone, Debug, PartialEq)]
pub struct CheckIssue {
    pub path: Vec<crate::utils::zod::PathSegment>,
    pub message: String,
}

/// The checker half of `.check(ctx)`. An empty vec passes.
pub type CheckFn = fn(&serde_json::Value) -> Vec<CheckIssue>;

/// The transform half of `.transform`.
pub type TransformFn = fn(serde_json::Value) -> serde_json::Value;

/// The side-effect half of `.catch(ctx => { f(ctx.error); return undefined })`.
/// Receives the error the catch swallows; the resolution is always `undefined`.
pub type CatchUndefinedFn = fn(&super::ZodError);

/// Maps to: the declaration order of zod string checks. Each kind occurs
/// at most once in the CC schema chains carried here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StringCheck {
    Min,
    Length,
    Prefix,
    Suffix,
    Regex,
    Format,
}

/// A zod string format check. CC's tool schemas use exactly one (`.url()`), so
/// this stays a closed set rather than an open string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StringFormat {
    /// `z.string().url()` — projects `format: "uri"`, rejects with
    /// `invalid_format` / `"Invalid URL"`.
    Url,
}

impl StringFormat {
    /// The `format` value zod writes into the JSON Schema.
    pub fn json_schema_format(self) -> &'static str {
        match self {
            StringFormat::Url => "uri",
        }
    }

    /// The `format` value zod writes onto the issue — not the same string as
    /// the JSON Schema one.
    pub fn issue_format(self) -> &'static str {
        match self {
            StringFormat::Url => "url",
        }
    }

    /// The issue message, which for a format check is a fixed sentence rather
    /// than the generated "Invalid string: …" line.
    pub fn issue_message(self) -> &'static str {
        match self {
            StringFormat::Url => "Invalid URL",
        }
    }
}

impl Schema {
    fn record_string_check(&mut self, check: StringCheck) {
        if let Schema::String { check_order, .. } = self
            && !check_order.contains(&check)
        {
            check_order.push(check);
        }
    }

    /// Maps to: zod/v4 `.min(n)` — string length or array item count depending
    /// on the receiver, as zod's own `.min` does.
    pub fn min(mut self, n: u64) -> Self {
        self.record_string_check(StringCheck::Min);
        match &mut self {
            Schema::String { min_length, .. } => *min_length = Some(n),
            Schema::Array { min_items, .. } => *min_items = Some(n),
            Schema::Number { min, .. } => *min = Some(n as f64),
            _ => {}
        }
        self
    }

    /// Maps to: zod/v4 `.min(n, message)` — the two-argument form, where the
    /// message replaces the generated "Too small: …" text. Works on strings
    /// and arrays (`AllowedMcpServerEntrySchema`'s serverCommand).
    pub fn min_with_message(mut self, n: u64, message: &'static str) -> Self {
        self.record_string_check(StringCheck::Min);
        match &mut self {
            Schema::String {
                min_length,
                min_message,
                ..
            } => {
                *min_length = Some(n);
                *min_message = Some(message);
            }
            Schema::Array {
                min_items,
                min_message,
                ..
            } => {
                *min_items = Some(n);
                *min_message = Some(message);
            }
            _ => {}
        }
        self
    }

    /// Maps to: zod/v4 `.length(n)` — an exact string-length check (`gitSha`'s
    /// `.length(40)`), projecting `minLength` AND `maxLength`.
    pub fn length(mut self, n: u64) -> Self {
        self.record_string_check(StringCheck::Length);
        if let Schema::String { length, .. } = &mut self {
            *length = Some(n);
        }
        self
    }

    /// Maps to: zod/v4 `.startsWith(p)` — a prefix check (`RelativePath`'s
    /// `./`), projecting an anchored escaped-prefix `pattern`.
    pub fn starts_with(mut self, prefix: &'static str) -> Self {
        self.record_string_check(StringCheck::Prefix);
        if let Schema::String { starts_with, .. } = &mut self {
            *starts_with = Some(prefix);
        }
        self
    }

    /// Maps to: zod/v4 `.startsWith(prefix, message)`.
    pub fn starts_with_message(mut self, prefix: &'static str, message: &'static str) -> Self {
        self.record_string_check(StringCheck::Prefix);
        if let Schema::String {
            starts_with,
            starts_with_message,
            ..
        } = &mut self
        {
            *starts_with = Some(prefix);
            *starts_with_message = Some(message);
        }
        self
    }

    /// Maps to: zod/v4 `.endsWith(suffix)`.
    pub fn ends_with(mut self, suffix: &'static str) -> Self {
        self.record_string_check(StringCheck::Suffix);
        if let Schema::String { ends_with, .. } = &mut self {
            *ends_with = Some(suffix);
        }
        self
    }

    /// Maps to: zod/v4 `.regex(RegExp(source, flags))`. This carrier supports
    /// the non-Unicode ASCII-pattern forms used by plugin schemas: no flags or `i`.
    /// Keeping the source separate preserves issue `/source/i` and JSON Schema source.
    pub fn regex_with_flags(mut self, pattern: &'static str, flags: &'static str) -> Self {
        self.record_string_check(StringCheck::Regex);
        assert!(
            flags.is_empty() || flags == "i",
            "unsupported JavaScript RegExp flags"
        );
        if let Schema::String {
            pattern: source,
            pattern_flags,
            ..
        } = &mut self
        {
            *source = Some(pattern);
            *pattern_flags = Some(flags);
        }
        self
    }

    /// Maps to: zod/v4 `.regex(RegExp(source, flags), message)`.
    pub fn regex_with_flags_and_message(
        self,
        pattern: &'static str,
        flags: &'static str,
        message: &'static str,
    ) -> Self {
        self.regex_with_flags(pattern, flags)
            .regex_with_message(pattern, message)
    }

    /// Maps to: zod/v4 `.regex(re, message)` — the two-argument form, where
    /// the message replaces the generated `invalid_format` text.
    pub fn regex_with_message(mut self, pattern: &'static str, message: &'static str) -> Self {
        self.record_string_check(StringCheck::Regex);
        if let Schema::String {
            pattern: p,
            pattern_message,
            ..
        } = &mut self
        {
            *p = Some(pattern);
            *pattern_message = Some(message);
        }
        self
    }

    /// Maps to: zod/v4 `.max(n)` on an array or a number.
    pub fn max(mut self, n: u64) -> Self {
        match &mut self {
            Schema::Array { max_items, .. } => *max_items = Some(n),
            Schema::Number { max, .. } => *max = Some(n as f64),
            _ => {}
        }
        self
    }

    /// Maps to: zod/v4 `.regex(re)`. Pass the pattern WITHOUT `/` delimiters —
    /// that is what JSON Schema carries; the issue message re-adds them.
    pub fn regex(mut self, pattern: &'static str) -> Self {
        self.record_string_check(StringCheck::Regex);
        if let Schema::String { pattern: p, .. } = &mut self {
            *p = Some(pattern);
        }
        self
    }

    /// Maps to: zod/v4 `.url()`.
    pub fn url(mut self) -> Self {
        self.record_string_check(StringCheck::Format);
        if let Schema::String { format, .. } = &mut self {
            *format = Some(StringFormat::Url);
        }
        self
    }

    /// Maps to: zod/v4 `.int()`.
    pub fn int(mut self) -> Self {
        if let Schema::Number { int, .. } = &mut self {
            *int = true;
        }
        self
    }

    /// Maps to: zod/v4 `.positive()`.
    pub fn positive(mut self) -> Self {
        if let Schema::Number { positive, .. } = &mut self {
            *positive = true;
        }
        self
    }

    /// Maps to: zod/v4 `.nonnegative()`.
    pub fn nonnegative(mut self) -> Self {
        if let Schema::Number { nonnegative, .. } = &mut self {
            *nonnegative = true;
        }
        self
    }

    /// Maps to: zod/v4 `.optional()`.
    pub fn optional(self) -> Self {
        Schema::Optional(Box::new(self))
    }

    /// Maps to: zod/v4 `.nullable()`.
    pub fn nullable(self) -> Self {
        Schema::Nullable(Box::new(self))
    }

    /// Maps to: zod/v4 `.default(v)`.
    pub fn default(self, value: serde_json::Value) -> Self {
        Schema::Default(Box::new(self), value)
    }

    /// Maps to: zod/v4 `.describe(text)`.
    pub fn describe(self, text: impl Into<String>) -> Self {
        Schema::Describe(Box::new(self), text.into())
    }

    /// Maps to: zod/v4 `.refine(f, {message})`.
    pub fn refine(self, f: RefineFn, message: &'static str) -> Self {
        Schema::Refine(f, message, Box::new(self))
    }

    /// Maps to: zod/v4 `.strict()` — reissues an object schema with unknown
    /// keys rejected (`validateSettingsFileContent` applies it to the
    /// passthrough SettingsSchema). No-op on non-object schemas.
    pub fn strict(self) -> Self {
        match self {
            Schema::PassthroughObject(shape) | Schema::Object(shape) => Schema::StrictObject(shape),
            other => other,
        }
    }

    /// Maps to: zod/v4 `.superRefine((v, ctx) => ...)`.
    pub fn super_refine(self, f: SuperRefineFn) -> Self {
        Schema::SuperRefine(f, Box::new(self))
    }

    /// Maps to: zod/v4 `.transform(f)`.
    pub fn transform(self, f: TransformFn) -> Self {
        Schema::Transform(f, Box::new(self))
    }

    /// Maps to: zod/v4 `.catch(undefined)`.
    pub fn catch_undefined(self) -> Self {
        Schema::CatchUndefined(Box::new(self))
    }

    /// Maps to: zod/v4 `.catch(fn)` where the fallback is `undefined` and the
    /// callback's only work is a side effect on `ctx.error`
    /// (`PermissionPromptToolResultSchema.ts:53-59`).
    pub fn catch_undefined_with(self, f: CatchUndefinedFn) -> Self {
        Schema::CatchUndefinedWith(f, Box::new(self))
    }

    /// Maps to: zod/v4 `.check(ctx => ...)`.
    pub fn check(self, f: CheckFn) -> Self {
        Schema::Check(f, Box::new(self))
    }
}

/// Maps to: zod/v4 `z.string()`.
pub fn string() -> Schema {
    Schema::String {
        check_order: Vec::new(),
        min_length: None,
        min_message: None,
        length: None,
        starts_with: None,
        starts_with_message: None,
        ends_with: None,
        pattern_flags: None,
        pattern: None,
        pattern_message: None,
        format: None,
    }
}

/// Maps to: zod/v4 `z.number()`.
pub fn number() -> Schema {
    Schema::Number {
        int: false,
        positive: false,
        nonnegative: false,
        min: None,
        max: None,
    }
}

/// Maps to: zod/v4 `z.boolean()`.
pub fn boolean() -> Schema {
    Schema::Boolean
}

/// Maps to: zod/v4 `z.literal(v)`.
pub fn literal(value: serde_json::Value) -> Schema {
    Schema::Literal(value)
}

/// Maps to: zod/v4 `z.enum([...])`.
pub fn enumeration(values: Vec<&'static str>) -> Schema {
    Schema::Enum(values)
}

/// Maps to: zod/v4 `z.array(T)`.
pub fn array(inner: Schema) -> Schema {
    Schema::Array {
        items: Box::new(inner),
        min_items: None,
        min_message: None,
        max_items: None,
    }
}

/// Maps to: zod/v4 `z.record(z.string(), T)`.
pub fn record(inner: Schema) -> Schema {
    Schema::Record(Box::new(inner))
}

/// Maps to: zod/v4 `z.record(keySchema, valueSchema)` for validated string keys.
pub fn record_with_key(key: Schema, value: Schema) -> Schema {
    Schema::RecordWithKey {
        key: Box::new(key),
        value: Box::new(value),
    }
}

/// Maps to: zod/v4 `z.partialRecord(z.enum(keys), T)` — CC's only key shape
/// for a partialRecord is an enum, so the carrier takes the options directly.
pub fn partial_record(keys: Vec<&'static str>, value: Schema) -> Schema {
    Schema::PartialRecord {
        keys,
        value: Box::new(value),
    }
}

/// Maps to: zod/v4 `z.any()`.
pub fn any() -> Schema {
    Schema::Any
}

/// Maps to: zod/v4 `z.strictObject({...})`.
pub fn strict_object(fields: Vec<ObjectField>) -> Schema {
    Schema::StrictObject(fields.into_iter().collect())
}

/// Maps to: zod/v4 `z.object({...})`.
pub fn object(fields: Vec<ObjectField>) -> Schema {
    Schema::Object(fields.into_iter().collect())
}

/// Maps to: zod/v4 `z.object({...}).passthrough()`.
pub fn passthrough_object(fields: Vec<ObjectField>) -> Schema {
    Schema::PassthroughObject(fields.into_iter().collect())
}

/// Maps to: zod/v4 `z.union([...])`.
pub fn union(variants: Vec<Schema>) -> Schema {
    Schema::Union(variants)
}

/// Maps to: zod/v4 `z.discriminatedUnion(tag, [...])`.
pub fn discriminated_union(tag: &'static str, variants: Vec<Schema>) -> Schema {
    Schema::DiscriminatedUnion { tag, variants }
}

/// Maps to: zod/v4 `z.preprocess(f, inner)`.
pub fn preprocess(f: PreprocessFn, inner: Schema) -> Schema {
    Schema::Preprocess(f, Box::new(inner))
}

/// Maps to: zod/v4 `z.coerce.string()` — `String(value)` then string
/// validation. The ECMAScript `String()` conversion: numbers through the
/// carrier's `Number::toString` port, booleans/null to their literals, arrays
/// joined by comma with null/undefined elements empty, objects to
/// `[object Object]`. Projection is the inner string schema, exactly as zod
/// projects coerce.
pub fn coerce_string() -> Schema {
    preprocess(js_string_conversion, string())
}

fn js_string_conversion(value: &serde_json::Value) -> serde_json::Value {
    serde_json::Value::String(js_string(value))
}

/// Maps to: the ECMAScript `String(value)` / `` `${value}` `` conversion.
///
/// `z.coerce.string()` is only the first consumer; every CC site that pushes a
/// JSON value through `String(...)` or a template literal lands here —
/// `tools/MCPTool/UI.tsx:165` `String(item.text)`, `:306`/`:351`
/// `String(value)`, `services/mcp/client.ts:2505` `String(resultContent.data)`,
/// `:2670` `String(result.toolResult)`. Hand-rolling it again re-introduces the
/// three divergences serde_json has from JS: objects stringify to
/// `[object Object]` (not JSON), arrays join with `,` (not JSON), and numbers go
/// through `Number::toString` (`1` — not serde_json's `1.0`).
pub fn js_string(value: &serde_json::Value) -> String {
    js_string_element(value, false)
}

/// `nested_array_element` carries `Array.prototype.join`'s one deviation from
/// `String()`: `null`/`undefined` elements join as the empty string.
fn js_string_element(value: &serde_json::Value, nested_array_element: bool) -> String {
    match value {
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Number(number) => number
            .as_f64()
            .map(crate::utils::zod::javascript_number_to_string)
            .unwrap_or_else(|| number.to_string()),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Null if nested_array_element => String::new(),
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| js_string_element(value, true))
            .collect::<Vec<_>>()
            .join(","),
        serde_json::Value::Object(_) => "[object Object]".to_string(),
    }
}

#[cfg(test)]
mod js_string_tests {
    use super::js_string;
    use serde_json::json;

    /// Golden values sampled from real `String(v)` in a JS engine (bun). Three
    /// consumers outside zod now depend on these exact strings
    /// (`tools/mcp_tool/ui.rs`, `services/mcp/client.rs`), and each of them had
    /// previously hand-rolled a serde_json-flavoured version that got all three
    /// non-string cases wrong.
    #[test]
    fn matches_ecmascript_string_conversion() {
        assert_eq!(js_string(&json!("text")), "text");
        assert_eq!(js_string(&json!(1.0)), "1");
        assert_eq!(js_string(&json!(1.5)), "1.5");
        assert_eq!(js_string(&json!(1e21)), "1e+21");
        assert_eq!(js_string(&json!(true)), "true");
        assert_eq!(js_string(&json!(null)), "null");
        assert_eq!(js_string(&json!({"a": 1})), "[object Object]");
        assert_eq!(js_string(&json!([1, 2])), "1,2");
        // `Array.prototype.join` renders null/undefined elements as empty, and
        // nested arrays flatten through their own join.
        assert_eq!(js_string(&json!([1, null, [2, 3]])), "1,,2,3");
        assert_eq!(js_string(&json!([])), "");
        assert_eq!(js_string(&json!([{"a": 1}])), "[object Object]");
    }
}
