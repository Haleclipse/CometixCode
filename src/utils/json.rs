//! Maps to: CC `utils/json.ts`.
//!
//! The current `/branch` consumer reads a Buffer. Its Bun runtime uses
//! `Bun.JSONL.parseChunk`, not the Node fallback's per-line `JSON.parse(trim())`.

use serde::Deserialize;
use serde_json::Value;

use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    sync::{Arc, LazyLock},
};

/// Maps to: CC `utils/json.ts#CachedParse`.
/// Arc preserves the shared object returned by cached JSON.parse results.
#[derive(Clone, Debug)]
pub enum CachedParse {
    Ok { value: Arc<Value> },
    Err,
}

/// Maps to: CC `utils/json.ts#parseJSONCached`.
static PARSE_JSON_CACHED: LazyLock<
    crate::utils::memoize::LruMemoizedFunction<(String, bool), CachedParse>,
> = LazyLock::new(|| {
    crate::utils::memoize::memoize_with_lru(
        |args: &(String, bool)| parse_json_uncached(&args.0, args.1),
        |args| args.0.clone(),
        Some(50),
    )
});

/// Maps to: CC `utils/json.ts#safeParseJSON.cache`: a reference to the same
/// cache exposed by parseJSONCached, not a second cache or an API-owned cache.
pub static SAFE_PARSE_JSON_CACHE: LazyLock<
    &'static crate::utils::memoize::LruMemoizedCache<CachedParse>,
> = LazyLock::new(|| &PARSE_JSON_CACHED.cache);

// Despite the source constant's name, its guard uses JS string.length (UTF-16).
const PARSE_CACHE_MAX_KEY_BYTES: usize = 8 * 1024;

/// Maps to: CC `utils/json.ts#parseJSONUncached`.
fn parse_json_uncached(json: &str, should_log_error: bool) -> CachedParse {
    // CC imports the leaf utils/jsonRead.ts#stripBOM: strip exactly one leading BOM.
    match serde_json::from_str(json.strip_prefix('\u{feff}').unwrap_or(json)) {
        Ok(value) => CachedParse::Ok {
            value: Arc::new(value),
        },
        Err(error) => {
            if should_log_error {
                let mut error = crate::utils::log::LogError::new(error.to_string());
                error.name = "SyntaxError".into();
                error.stack = error
                    .stack
                    .map(|stack| stack.replacen("Error:", "SyntaxError:", 1));
                crate::utils::log::log_error(error);
            }
            CachedParse::Err
        }
    }
}

/// Maps to: CC `utils/json.ts#safeParseJSON`.
/// None represents the source's null/undefined input; callers explicitly pass
/// the source default logging flag (true). Returned JSON null covers all errors.
pub fn safe_parse_json(json: Option<&str>, should_log_error: bool) -> Arc<Value> {
    let Some(json) = json.filter(|json| !json.is_empty()) else {
        return Arc::new(Value::Null);
    };
    let result = if json.encode_utf16().count() > PARSE_CACHE_MAX_KEY_BYTES {
        parse_json_uncached(json, should_log_error)
    } else {
        PARSE_JSON_CACHED.call(&(json.to_owned(), should_log_error))
    };
    match result {
        CachedParse::Ok { value } => value,
        CachedParse::Err => Arc::new(Value::Null),
    }
}

/// Maps to: CC `utils/json.ts#safeParseJSONC:65–78`.
/// `None` carries JavaScript `undefined` (no root value), distinct from JSON null.
pub fn safe_parse_jsonc(json: &str) -> Option<JsoncValue> {
    if json.is_empty() {
        return Some(JsoncValue::null());
    }
    let clean = json.strip_prefix('\u{feff}').unwrap_or(json);
    JsoncParser::new(clean).parse_value()
}

/// Maps to: CC `utils/json.ts#addItemToJSONCArray:228–277`.
pub fn add_item_to_jsonc_array(content: &str, new_item: &Value) -> String {
    let fallback =
        || crate::utils::slow_operations::json_stringify(&Value::Array(vec![new_item.clone()]), 4);
    let clean = content.strip_prefix('\u{feff}').unwrap_or(content);
    let Some(root) = JsoncParser::new(clean).parse_value() else {
        return fallback();
    };
    if !root.value.is_array() {
        return fallback();
    }
    // Source modify([array.length], value, {isArrayInsertion:true}) always
    // inserts immediately after the final child, before existing tail trivia.
    let (offset, prefix) = root
        .children
        .last()
        .map_or((root.start + 1, ""), |last| (last.end, ","));
    let mut units: Vec<u16> = clean.encode_utf16().collect();
    if offset > units.len() {
        // Source scanner may run one code unit past EOF after an unterminated
        // CRLF block comment. applyEdits rejects that out-of-document range.
        crate::utils::log::log_error(crate::utils::log::LogError::new("Overlapping edit"));
        return fallback();
    }
    let insertion: Vec<u16> = format!(
        "{prefix}{}",
        crate::utils::slow_operations::json_stringify(new_item, 0)
    )
    .encode_utf16()
    .collect();
    let mut begin = offset;
    let mut end = offset + insertion.len();
    units.splice(offset..offset, insertion);
    while begin > 0 && !matches!(units[begin - 1], 10 | 13) {
        begin -= 1;
    }
    while end < units.len() && !matches!(units[end], 10 | 13) {
        end += 1;
    }
    let edits = match jsonc_format(&units, begin, end) {
        Ok(edits) => edits,
        Err(()) => {
            let mut error = crate::utils::log::LogError::new(
                "undefined is not an object (evaluating 'edit.content.length')",
            );
            error.name = "TypeError".into();
            error.stack = error
                .stack
                .map(|stack| stack.replacen("Error:", "TypeError:", 1));
            crate::utils::log::log_error(error);
            return fallback();
        }
    };
    for (start, end, text) in edits.into_iter().rev() {
        units.splice(start..end, text);
    }
    String::from_utf16_lossy(&units)
}

// The following private scanner/parser/formatter are the subset required by
// the two CC wrappers above, translated from Microsoft node-jsonc-parser 3.3.1:
// lib/esm/impl/scanner.js#createScanner, parser.js#visit/parse/parseTree,
// edit.js#setProperty (array insertion)/withFormatting, format.js#format.
// They are dependency adaptations, NOT functions defined in CC utils/json.ts.
// All text positions remain UTF-16 code-unit offsets until final Rust String
// conversion. No comments are removed or untouched file regions reserialized.
//
// The MIT License (MIT)
// Copyright (c) Microsoft
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

#[derive(Clone)]
struct JsoncToken {
    kind: u8,
    start: usize,
    end: usize,
    value: Vec<u16>,
    error: bool,
}

struct JsoncScanner<'a> {
    text: &'a [u16],
    position: usize,
}

impl<'a> JsoncScanner<'a> {
    fn new(text: &'a [u16]) -> Self {
        Self { text, position: 0 }
    }
    fn at(&self, position: usize) -> u16 {
        self.text.get(position).copied().unwrap_or(u16::MAX)
    }
    fn digit(c: u16) -> bool {
        (48..=57).contains(&c)
    }
    fn scan(&mut self) -> JsoncToken {
        let start = self.position;
        let mut token = JsoncToken {
            kind: 16,
            start,
            end: start,
            value: Vec::new(),
            error: false,
        };
        let ch = self.at(start);
        if start >= self.text.len() {
            token.kind = 17;
            token.start = self.text.len();
            return token;
        }
        self.position += 1;
        match ch {
            32 | 9 => {
                token.kind = 15;
                while matches!(self.at(self.position), 32 | 9) {
                    self.position += 1;
                }
            }
            10 | 13 => {
                token.kind = 14;
                if ch == 13 && self.at(self.position) == 10 {
                    self.position += 1;
                }
            }
            123 => token.kind = 1,
            125 => token.kind = 2,
            91 => token.kind = 3,
            93 => token.kind = 4,
            44 => token.kind = 5,
            58 => token.kind = 6,
            34 => {
                token.kind = 10;
                loop {
                    let c = self.at(self.position);
                    if self.position >= self.text.len() || matches!(c, 10 | 13) {
                        token.error = true;
                        break;
                    }
                    self.position += 1;
                    if c == 34 {
                        break;
                    }
                    if c == 92 {
                        if self.position >= self.text.len() {
                            token.error = true;
                            break;
                        }
                        let escaped = self.at(self.position);
                        self.position += 1;
                        match escaped {
                            34 | 92 | 47 => token.value.push(escaped),
                            98 => token.value.push(8),
                            102 => token.value.push(12),
                            110 => token.value.push(10),
                            114 => token.value.push(13),
                            116 => token.value.push(9),
                            117 => {
                                let mut value = 0u16;
                                let mut digits = 0;
                                while digits < 4 {
                                    let c = self.at(self.position);
                                    let digit = match c {
                                        48..=57 => c - 48,
                                        65..=70 => c - 65 + 10,
                                        97..=102 => c - 97 + 10,
                                        _ => break,
                                    };
                                    value = value * 16 + digit;
                                    self.position += 1;
                                    digits += 1;
                                }
                                if digits == 4 {
                                    token.value.push(value);
                                } else {
                                    token.error = true;
                                }
                            }
                            _ => token.error = true,
                        }
                    } else {
                        if c <= 31 {
                            token.error = true;
                        }
                        token.value.push(c);
                    }
                }
            }
            47 if self.at(self.position) == 47 => {
                token.kind = 12;
                self.position += 1;
                while self.position < self.text.len() && !matches!(self.at(self.position), 10 | 13)
                {
                    self.position += 1;
                }
            }
            47 if self.at(self.position) == 42 => {
                token.kind = 13;
                self.position += 1;
                while self.position + 1 < self.text.len()
                    && !(self.at(self.position) == 42 && self.at(self.position + 1) == 47)
                {
                    let c = self.at(self.position);
                    self.position += 1;
                    if c == 13 && self.at(self.position) == 10 {
                        self.position += 1;
                    }
                }
                if self.position + 1 < self.text.len() {
                    self.position += 2;
                } else {
                    self.position += 1;
                    token.error = true;
                }
            }
            47 => {}
            45 | 48..=57 => {
                if ch == 45 && !Self::digit(self.at(self.position)) {
                    token.end = self.position;
                    return token;
                }
                self.position = start + usize::from(ch == 45);
                if self.at(self.position) == 48 {
                    self.position += 1;
                } else {
                    while Self::digit(self.at(self.position)) {
                        self.position += 1;
                    }
                }
                if self.at(self.position) == 46 {
                    self.position += 1;
                    if !Self::digit(self.at(self.position)) {
                        token.error = true;
                    }
                    while Self::digit(self.at(self.position)) {
                        self.position += 1;
                    }
                }
                let mut end = self.position;
                if !token.error && matches!(self.at(self.position), 69 | 101) {
                    self.position += 1;
                    if matches!(self.at(self.position), 43 | 45) {
                        self.position += 1;
                    }
                    if Self::digit(self.at(self.position)) {
                        while Self::digit(self.at(self.position)) {
                            self.position += 1;
                        }
                        end = self.position;
                    } else {
                        token.error = true;
                    }
                }
                token.kind = 11;
                token.value = self.text[start..end].to_vec();
            }
            _ => {
                while self.position < self.text.len()
                    && !matches!(
                        self.at(self.position),
                        32 | 9 | 10 | 13 | 125 | 93 | 123 | 91 | 34 | 58 | 44 | 47
                    )
                {
                    self.position += 1;
                }
                token.kind =
                    match String::from_utf16_lossy(&self.text[start..self.position]).as_str() {
                        "null" => 7,
                        "true" => 8,
                        "false" => 9,
                        _ => 16,
                    };
            }
        }
        token.end = self.position;
        token
    }
}

/// Necessary dynamic-value carrier for CC safeParseJSONC's JavaScript result.
/// Unlike serde Value, a parsed nonfinite number is distinct from null, and
/// object lookup can observe prototype setters in node-jsonc-parser#parse.
/// Offsets and child nodes are shared with modify's parseTree adapter.
#[derive(Clone)]
pub struct JsoncValue {
    start: usize,
    end: usize,
    pub(crate) kind: u8,
    pub(crate) number: Option<f64>,
    pub(crate) string_units: Vec<u16>,
    value: Value,
    children: Vec<JsoncValue>,
    pub(crate) properties: Vec<(Vec<u16>, JsoncValue)>,
    prototype: Option<Box<JsoncValue>>,
    object_prototype: bool,
}

impl JsoncValue {
    fn null() -> Self {
        Self {
            start: 0,
            end: 0,
            kind: 7,
            number: None,
            string_units: Vec::new(),
            value: Value::Null,
            children: Vec::new(),
            properties: Vec::new(),
            prototype: None,
            object_prototype: true,
        }
    }
    /// Rust JSON input adapter; already-representable values do not need a
    /// second parse or serialization round trip to enter the dynamic carrier.
    pub fn from_json(value: Value) -> Self {
        let mut result = Self::null();
        match &value {
            Value::Null => {}
            Value::Bool(v) => result.kind = if *v { 8 } else { 9 },
            Value::Number(v) => {
                result.kind = 11;
                result.number = v.as_f64();
            }
            Value::String(v) => {
                result.kind = 10;
                result.string_units = v.encode_utf16().collect();
            }
            Value::Array(v) => {
                return Self::array(v.iter().cloned().map(Self::from_json).collect());
            }
            Value::Object(v) => {
                result.kind = 1;
                result.properties = v
                    .iter()
                    .map(|(key, value)| {
                        (key.encode_utf16().collect(), Self::from_json(value.clone()))
                    })
                    .collect();
            }
        }
        result.value = value;
        result
    }
    pub fn array(children: Vec<Self>) -> Self {
        let mut value = Self::null();
        value.kind = 3;
        value.value = Value::Array(children.iter().map(Self::to_json).collect());
        value.properties.push((
            "length".encode_utf16().collect(),
            Self::from_json(Value::from(children.len())),
        ));
        value.children = children;
        value
    }
    pub fn into_array(self) -> Option<Vec<Self>> {
        (self.kind == 3).then_some(self.children)
    }
    fn has_proto_setter(&self) -> bool {
        if self
            .properties
            .iter()
            .any(|(name, _)| name == &"__proto__".encode_utf16().collect::<Vec<_>>())
        {
            return false;
        }
        self.prototype
            .as_ref()
            .map_or(self.object_prototype, |prototype| {
                prototype.has_proto_setter()
            })
    }
    pub fn is_null(&self) -> bool {
        self.kind == 7
    }
    pub fn as_array(&self) -> Option<&[Self]> {
        (self.kind == 3).then_some(self.children.as_slice())
    }
    pub fn as_str(&self) -> Option<&str> {
        self.value.as_str()
    }
    pub fn get_property(&self, key: &str) -> Option<&Self> {
        self.get_property_units(&key.encode_utf16().collect::<Vec<_>>())
    }
    fn get_property_units(&self, key: &[u16]) -> Option<&Self> {
        if let Some((_, value)) = self.properties.iter().find(|(name, _)| name == key) {
            return Some(value);
        }
        if self.kind == 3 {
            let name = String::from_utf16_lossy(key);
            if let Ok(index) = name.parse::<usize>() {
                if index.to_string() == name {
                    if let Some(child) = self.children.get(index) {
                        return Some(child);
                    }
                }
            }
        }
        self.prototype
            .as_ref()
            .and_then(|prototype| prototype.get_property_units(key))
    }
    fn has_array_builtin(&self, method: &str) -> bool {
        if self
            .properties
            .iter()
            .any(|(name, _)| name == &method.encode_utf16().collect::<Vec<_>>())
        {
            return false;
        }
        self.kind == 3
            || self
                .prototype
                .as_ref()
                .is_some_and(|prototype| prototype.has_array_builtin(method))
    }
    /// Necessary JS Array.prototype.find receiver adapter for terminalSetup's
    /// actual `.find` call: an object can inherit the builtin from an array.
    /// JSON data can shadow it but cannot contain another callable function.
    pub fn array_find_length(&self) -> Result<u64, ()> {
        if !self.has_array_builtin("find") {
            return Err(());
        }
        self.arraylike_length()
    }
    fn arraylike_length(&self) -> Result<u64, ()> {
        let number = self
            .get_property("length")
            .map_or(Ok(f64::NAN), |value| value.to_number())?;
        if number.is_nan() || number <= 0.0 {
            return Ok(0);
        }
        Ok(number.floor().min(9_007_199_254_740_991.0) as u64)
    }
    pub fn array_find_item(&self, index: u64) -> Option<&Self> {
        self.get_property(&index.to_string())
    }
    pub(crate) fn to_number(&self) -> Result<f64, ()> {
        match self.kind {
            7 => Ok(0.0),
            8 => Ok(1.0),
            9 => Ok(0.0),
            11 => Ok(self.number.unwrap_or(f64::NAN)),
            10 => Ok(
                crate::utils::read_file_in_range::javascript_string_to_number(
                    self.as_str().unwrap_or(""),
                ),
            ),
            _ => Ok(
                crate::utils::read_file_in_range::javascript_string_to_number(
                    &self.array_string()?,
                ),
            ),
        }
    }
    fn has_object_prototype(&self) -> bool {
        self.prototype
            .as_ref()
            .map_or(self.object_prototype, |prototype| {
                prototype.has_object_prototype()
            })
    }
    pub(crate) fn array_string(&self) -> Result<String, ()> {
        Ok(match self.kind {
            7 => String::new(),
            10 => self.as_str().unwrap_or("").to_owned(),
            11 => ryu_js::Buffer::new()
                .format(self.number.unwrap_or(f64::NAN))
                .to_owned(),
            1 | 3 => {
                if self.get_property("toString").is_some() {
                    return Err(());
                }
                if self.has_array_builtin("toString") && self.has_array_builtin("join") {
                    let mut text = String::new();
                    for index in 0..self.arraylike_length()? {
                        if index > 0 {
                            text.push(',');
                        }
                        if let Some(item) = self.array_find_item(index) {
                            text.push_str(&item.array_string()?);
                        }
                    }
                    text
                } else if self.has_object_prototype() {
                    "[object Object]".into()
                } else {
                    return Err(());
                }
            }
            _ => self.value.to_string(),
        })
    }
    /// Explicit projection into the project's existing JSON carrier: JS
    /// nonfinite numbers serialize as null; lone surrogates require U+FFFD.
    /// Installers inspect this dynamic carrier before any lossy projection.
    pub fn to_json(&self) -> Value {
        self.value.clone()
    }
}

struct JsoncParser {
    tokens: Vec<JsoncToken>,
    index: usize,
    strict_objects: bool,
}

impl JsoncParser {
    fn new(text: &str) -> Self {
        let units: Vec<u16> = text.encode_utf16().collect();
        let mut scanner = JsoncScanner::new(&units);
        let mut tokens = Vec::new();
        loop {
            let token = scanner.scan();
            let eof = token.kind == 17;
            if token.kind < 12 || eof {
                tokens.push(token);
            }
            if eof {
                break;
            }
        }
        Self {
            tokens,
            index: 0,
            strict_objects: false,
        }
    }
    fn token(&self) -> &JsoncToken {
        &self.tokens[self.index]
    }
    fn advance(&mut self) {
        if self.token().kind != 17 {
            self.index += 1;
        }
    }
    fn recover(&mut self, close: u8) {
        while ![close, 5, 17].contains(&self.token().kind) {
            self.advance();
        }
    }
    fn parse_value(&mut self) -> Option<JsoncValue> {
        let token = self.token().clone();
        let mut children = Vec::new();
        let mut numeric_value = None;
        let mut properties: Vec<(Vec<u16>, JsoncValue)> = Vec::new();
        let mut prototype: Option<Box<JsoncValue>> = None;
        let mut object_prototype = true;
        let value = match token.kind {
            1 | 3 => {
                let array = token.kind == 3;
                let close = if array { 4 } else { 2 };
                self.advance();
                let mut object = serde_json::Map::new();
                while self.token().kind != close && self.token().kind != 17 {
                    if self.token().kind == 5 {
                        self.advance();
                    }
                    if array {
                        if let Some(child) = self.parse_value() {
                            children.push(child);
                        } else {
                            self.recover(close);
                        }
                    } else if self.token().kind == 10 {
                        let key_units = self.token().value.clone();
                        let key = String::from_utf16_lossy(&key_units);
                        self.advance();
                        if self.token().kind == 6 {
                            self.advance();
                            if let Some(child) = self.parse_value() {
                                // JS ordinary object assignment treats __proto__ as
                                // a prototype setter rather than an own JSON property.
                                let has_proto_setter = !self.strict_objects
                                    && !properties.iter().any(|(name, _)| *name == key_units)
                                    && prototype.as_ref().map_or(object_prototype, |prototype| {
                                        prototype.has_proto_setter()
                                    });
                                if key == "__proto__" && has_proto_setter {
                                    if child.kind == 7 {
                                        prototype = None;
                                        object_prototype = false;
                                    } else if matches!(child.kind, 1 | 3) {
                                        prototype = Some(Box::new(child));
                                        object_prototype = false;
                                    }
                                } else {
                                    object.insert(key, child.value.clone());
                                    if let Some((_, previous)) =
                                        properties.iter_mut().find(|(name, _)| *name == key_units)
                                    {
                                        *previous = child;
                                    } else {
                                        properties.push((key_units, child));
                                    }
                                }
                            } else {
                                self.recover(close);
                            }
                        } else {
                            self.recover(close);
                        }
                    } else {
                        self.recover(close);
                    }
                }
                if array {
                    let mut length = JsoncValue::null();
                    length.kind = 11;
                    length.number = Some(children.len() as f64);
                    length.value = Value::from(children.len());
                    properties.push(("length".encode_utf16().collect(), length));
                }
                let end = self.token().end;
                self.advance();
                return Some(JsoncValue {
                    start: token.start,
                    end,
                    kind: token.kind,
                    number: numeric_value,
                    string_units: Vec::new(),
                    properties,
                    prototype,
                    object_prototype,
                    value: if array {
                        Value::Array(children.iter().map(|c| c.value.clone()).collect())
                    } else {
                        Value::Object(object)
                    },
                    children,
                });
            }
            10 => Value::String(String::from_utf16_lossy(&token.value)),
            11 => {
                let raw = String::from_utf16_lossy(&token.value);
                let number = raw.parse::<f64>().unwrap_or(0.0);
                numeric_value = Some(number);
                if number.is_finite()
                    && number.fract() == 0.0
                    && number >= i64::MIN as f64
                    && number < i64::MAX as f64
                {
                    Value::from(number as i64)
                } else {
                    serde_json::Number::from_f64(number).map_or(Value::Null, Value::Number)
                }
            }
            7 => Value::Null,
            8 => Value::Bool(true),
            9 => Value::Bool(false),
            _ => return None,
        };
        self.advance();
        Some(JsoncValue {
            start: token.start,
            end: token.end,
            kind: token.kind,
            number: numeric_value,
            string_units: if token.kind == 10 {
                token.value.clone()
            } else {
                Vec::new()
            },
            properties,
            prototype,
            object_prototype,
            value,
            children,
        })
    }
}

// serde IgnoredAny validation is performed by slow_operations::json_parse.
// Reuse the existing dependency scanner/materializer without tolerant grammar
// escaping that validator; JSON.parse creates an own __proto__ data property.
pub(crate) fn parse_json_value_after_validation(text: &str) -> JsoncValue {
    let mut parser = JsoncParser::new(text);
    parser.strict_objects = true;
    parser.parse_value().expect("validated JSON has a root")
}

// Dependency format.js#format with CC's fixed insertSpaces:true, tabSize:4,
// keepLines:false and range. Edits are UTF-16 spans in the original document.
fn jsonc_format(
    text: &[u16],
    begin: usize,
    end: usize,
) -> Result<Vec<(usize, usize, Vec<u16>)>, ()> {
    let eol = text
        .iter()
        .position(|c| matches!(c, 10 | 13))
        .map_or(vec![10], |i| {
            if text[i] == 13 && text.get(i + 1) == Some(&10) {
                vec![13, 10]
            } else {
                vec![text[i]]
            }
        });
    let initial_indent = text[begin..end]
        .iter()
        .take_while(|c| matches!(c, 9 | 32))
        .map(|c| if *c == 9 { 4 } else { 1 })
        .sum::<usize>()
        / 4;
    let mut scanner = JsoncScanner::new(&text[begin..end]);
    let scan_next = |scanner: &mut JsoncScanner<'_>| {
        let mut newline = false;
        loop {
            let token = scanner.scan();
            if token.kind == 14 {
                newline = true;
            } else if token.kind != 15 {
                return (token, newline);
            }
        }
    };
    let newline_indent = |indent: i32| {
        // Original cache arrays have length 200 but format.js uses `>`:
        // indexing exactly 200 yields JS undefined, only failing if an edit
        // containing it survives the range/error filters below.
        if (initial_indent as i32 + indent) * 4 == 200 {
            return None;
        }
        let mut value = eol.clone();
        value.extend(std::iter::repeat_n(
            32,
            ((initial_indent as i32 + indent).max(0) * 4) as usize,
        ));
        Some(value)
    };
    let mut edits = Vec::new();
    let mut add_edit = |value: Option<Vec<u16>>, start: usize, finish: usize, error: bool| {
        if !error
            && start < end
            && finish > begin
            && value
                .as_ref()
                .is_none_or(|value| text[start..finish] != *value)
        {
            edits.push((start, finish, value));
        }
    };
    let (mut first, _) = scan_next(&mut scanner);
    if first.kind != 17 {
        add_edit(
            Some(vec![32; initial_indent * 4]),
            begin,
            first.start + begin,
            first.error || first.kind == 16,
        );
    }
    let mut indent = 0i32;
    while first.kind != 17 {
        let mut first_end = first.end + begin;
        let (mut second, mut newline) = scan_next(&mut scanner);
        let mut error = second.error || second.kind == 16;
        let mut replace = Some(Vec::new());
        let mut needs_line_break = false;
        while !newline && matches!(second.kind, 12 | 13) {
            add_edit(Some(vec![32]), first_end, second.start + begin, error);
            first_end = second.end + begin;
            needs_line_break = second.kind == 12;
            replace = if needs_line_break {
                newline_indent(indent)
            } else {
                Some(Vec::new())
            };
            (second, newline) = scan_next(&mut scanner);
            error = second.error || second.kind == 16;
        }
        if second.kind == 2 || second.kind == 4 {
            let open = if second.kind == 2 { 1 } else { 3 };
            if first.kind != open {
                indent -= 1;
                replace = newline_indent(indent);
            }
        } else {
            match first.kind {
                1 | 3 => {
                    indent += 1;
                    replace = newline_indent(indent);
                }
                5 | 12 => replace = newline_indent(indent),
                13 => {
                    if newline {
                        replace = newline_indent(indent);
                    } else if !needs_line_break {
                        replace = Some(vec![32]);
                    }
                }
                6 if !needs_line_break => replace = Some(vec![32]),
                10 if second.kind == 6 && !needs_line_break => replace = Some(Vec::new()),
                2 | 4 | 7 | 8 | 9 | 11 => {
                    if matches!(second.kind, 12 | 13) && !needs_line_break {
                        replace = Some(vec![32]);
                    } else if !matches!(second.kind, 5 | 17) {
                        error = true;
                    }
                }
                16 => error = true,
                _ => {}
            }
            if newline && matches!(second.kind, 12 | 13) {
                replace = newline_indent(indent);
            }
        }
        if second.kind == 17 {
            replace = Some(Vec::new());
        }
        add_edit(replace, first_end, second.start + begin, error);
        first = second;
    }
    edits
        .into_iter()
        .map(|(start, end, text)| text.map(|text| (start, end, text)).ok_or(()))
        .collect()
}

/// Maps to: CC `utils/json.ts#parseJSONL`, Buffer input on the Bun runtime.
/// Raw values retain transcript metadata that typed message projection omits.
pub fn parse_jsonl(data: &[u8]) -> Vec<Value> {
    parse_jsonl_bun(data)
}

/// Maps to: CC `utils/json.ts#parseJSONLBun`.
/// `serde_json::Deserializer` supplies Bun's incremental JSON decoder boundary:
/// one value may span lines; after success the rest of that line is ignored;
/// after failure the caller retries from the next newline. Buffer decoding is
/// loss-tolerant UTF-8, and only its initial BOM is stripped.
fn parse_jsonl_bun(data: &[u8]) -> Vec<Value> {
    let data = data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(data);
    let decoded = String::from_utf8_lossy(data);
    let mut remaining = decoded.as_bytes();
    let mut values = Vec::new();
    while !remaining.is_empty() {
        let mut decoder = serde_json::Deserializer::from_slice(remaining);
        if let Ok(value) = Value::deserialize(&mut decoder) {
            let consumed = decoder.into_iter::<Value>().byte_offset();
            values.push(value);
            remaining = &remaining[consumed..];
        }
        let Some(newline) = remaining.iter().position(|byte| *byte == b'\n') else {
            break;
        };
        remaining = &remaining[newline + 1..];
    }
    values
}

const MAX_JSONL_READ_BYTES: usize = 100 * 1024 * 1024;

/// Maps to: CC `utils/json.ts#readJSONLFile`.
/// Synchronous I/O runs on the existing caller-owned stats/session worker;
/// the cap, tail offset, partial-line handling and propagated I/O errors match.
pub fn read_jsonl_file(file_path: impl AsRef<Path>) -> io::Result<Vec<Value>> {
    let file_path = file_path.as_ref();
    let size = std::fs::metadata(file_path)?.len();
    if size <= MAX_JSONL_READ_BYTES as u64 {
        return Ok(parse_jsonl(&std::fs::read(file_path)?));
    }
    let mut file = File::open(file_path)?;
    let mut buf = vec![0u8; MAX_JSONL_READ_BYTES];
    let mut total_read = 0;
    file.seek(SeekFrom::Start(size - MAX_JSONL_READ_BYTES as u64))?;
    while total_read < MAX_JSONL_READ_BYTES {
        let bytes_read = file.read(&mut buf[total_read..])?;
        if bytes_read == 0 {
            break;
        }
        total_read += bytes_read;
    }
    let buf = &buf[..total_read];
    if let Some(newline_index) = buf.iter().position(|byte| *byte == b'\n') {
        if newline_index + 1 < total_read {
            return Ok(parse_jsonl(&buf[newline_index + 1..]));
        }
    }
    Ok(parse_jsonl(buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use serde_json::json;

    struct FixtureDirectory(std::path::PathBuf);

    impl FixtureDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("cometix-json-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for FixtureDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parse_jsonl_matches_official_bun_buffer_recovery_and_bom() {
        assert_eq!(
            parse_jsonl(b"\xef\xbb\xbf{\"a\":1}\nBAD\n{\"b\":2}"),
            vec![json!({"a": 1}), json!({"b": 2})]
        );
        assert_eq!(
            parse_jsonl(b"\n\t1\r\nnull\nfalse\n\"x\"\n[]"),
            vec![json!(1), Value::Null, json!(false), json!("x"), json!([])]
        );
        assert!(parse_jsonl(b"\nBAD\n").is_empty());
        assert_eq!(
            parse_jsonl(&[b'"', 0xff, b'"', b'\n', b'1']),
            vec![json!("\u{fffd}"), json!(1)]
        );
    }

    #[test]
    fn parse_jsonl_matches_official_bun_chunk_not_node_line_parser() {
        // Generated with the actual CC parseJSONL(Buffer.from(...)), Bun 1.3.14.
        assert_eq!(parse_jsonl(b"[1,\n2]\n3"), vec![json!([1, 2]), json!(3)]);
        assert_eq!(
            parse_jsonl(b"1 2\n{}{}\n12garbage\n3"),
            vec![json!(1), json!({}), json!(12), json!(3)]
        );
        assert_eq!(
            parse_jsonl(b"{\"a\":1}\n[2,\nBAD\n3]\n4"),
            vec![json!({"a": 1}), json!(3), json!(4)]
        );
        assert_eq!(
            parse_jsonl("1\n\u{feff}2\n\u{a0}3\n4".as_bytes()),
            vec![json!(1), json!(4)]
        );
    }

    #[test]
    fn safe_parse_json_matches_official_bom_null_and_shared_result() {
        // CC utils/json.ts:31–58; Bun oracle in .test/backlog-json-0912.
        SAFE_PARSE_JSON_CACHE.clear();
        for input in [
            None,
            Some(""),
            Some("null"),
            Some("{bad"),
            Some("\u{feff}\u{feff}{}"),
        ] {
            assert_eq!(*safe_parse_json(input, false), Value::Null);
        }
        assert!(matches!(
            SAFE_PARSE_JSON_CACHE.get("null"),
            Some(CachedParse::Ok { .. })
        ));
        assert!(matches!(
            SAFE_PARSE_JSON_CACHE.get("{bad"),
            Some(CachedParse::Err)
        ));
        let first = safe_parse_json(Some("\u{feff}{\"a\":1}"), false);
        let second = safe_parse_json(Some("\u{feff}{\"a\":1}"), true);
        assert_eq!(*first, json!({"a":1}));
        assert!(Arc::ptr_eq(&first, &second));
        assert!(!SAFE_PARSE_JSON_CACHE.has(""));
    }

    #[test]
    fn safe_parse_json_matches_official_lru_peek_and_utf16_limit() {
        // CC utils/json.ts:29,51–55 and memoize.ts:246–266.
        SAFE_PARSE_JSON_CACHE.clear();
        for value in 0..50 {
            safe_parse_json(Some(&value.to_string()), false);
        }
        assert_eq!(SAFE_PARSE_JSON_CACHE.size(), 50);
        assert!(SAFE_PARSE_JSON_CACHE.get("0").is_some());
        safe_parse_json(Some("50"), false);
        assert!(
            !SAFE_PARSE_JSON_CACHE.has("0"),
            "cache.get must not refresh LRU"
        );
        safe_parse_json(Some("1"), false);
        safe_parse_json(Some("51"), false);
        assert!(SAFE_PARSE_JSON_CACHE.has("1"));
        assert!(!SAFE_PARSE_JSON_CACHE.has("2"));
        assert!(SAFE_PARSE_JSON_CACHE.delete("1"));
        assert!(!SAFE_PARSE_JSON_CACHE.delete("1"));
        SAFE_PARSE_JSON_CACHE.clear();
        for sample in [
            format!("\"{}\"", "😀".repeat(4095)),
            format!("\"{}\"", "中".repeat(8190)),
        ] {
            assert_eq!(sample.encode_utf16().count(), 8192);
            safe_parse_json(Some(&sample), false);
            assert!(SAFE_PARSE_JSON_CACHE.has(&sample));
        }
        let large = format!("\"{}\"", "中".repeat(8191));
        let first = safe_parse_json(Some(&large), false);
        let second = safe_parse_json(Some(&large), false);
        assert_eq!(first, second);
        assert!(!Arc::ptr_eq(&first, &second));
        assert!(!SAFE_PARSE_JSON_CACHE.has(&large));
    }

    #[test]
    fn safe_parse_json_matches_official_cached_failure_log_flag_order() {
        // CC utils/json.ts:13–26,31–42: failures are cached by json alone.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _env: Vec<_> = [
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "DISABLE_ERROR_REPORTING",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
        ]
        .into_iter()
        .map(EnvVarGuard::unset)
        .collect();
        crate::utils::log::_reset_error_log_for_testing();
        SAFE_PARSE_JSON_CACHE.clear();
        safe_parse_json(Some("{bad-first-silent"), false);
        safe_parse_json(Some("{bad-first-silent"), true);
        assert!(crate::utils::log::get_in_memory_errors().is_empty());
        safe_parse_json(Some("{bad-first-logged"), true);
        safe_parse_json(Some("{bad-first-logged"), true);
        assert_eq!(crate::utils::log::get_in_memory_errors().len(), 1);
        SAFE_PARSE_JSON_CACHE.delete("{bad-first-logged");
        safe_parse_json(Some("{bad-first-logged"), true);
        assert_eq!(crate::utils::log::get_in_memory_errors().len(), 2);
        let large_invalid = "?".repeat(8193);
        safe_parse_json(Some(&large_invalid), true);
        safe_parse_json(Some(&large_invalid), true);
        assert_eq!(crate::utils::log::get_in_memory_errors().len(), 4);
    }

    #[test]
    fn read_jsonl_file_matches_official_bun_and_io_errors() {
        // CC utils/json.ts:201–205 delegates the entire small Buffer to parseJSONL.
        let dir = FixtureDirectory::new();
        let path = dir.path().join("source.jsonl");
        std::fs::write(&path, b"\xef\xbb\xbf[1,\n2]\nBAD\n{\"tail\":true}").unwrap();
        assert_eq!(
            read_jsonl_file(&path).unwrap(),
            vec![json!([1, 2]), json!({"tail":true})]
        );
        assert_eq!(
            read_jsonl_file(dir.path().join("missing"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert!(read_jsonl_file(dir.path()).is_err());
    }

    #[test]
    fn read_jsonl_file_matches_official_100_mib_tail_partial_line() {
        // CC utils/json.ts:207–225: read last 100 MiB and skip its first line,
        // even when that line happens to be independently valid JSON.
        use std::io::Write;
        let dir = FixtureDirectory::new();
        let path = dir.path().join("large.jsonl");
        let mut file = File::create(&path).unwrap();
        file.set_len(MAX_JSONL_READ_BYTES as u64 + 8).unwrap();
        file.seek(SeekFrom::Start(8)).unwrap();
        file.write_all(b"123\n{\"tail\":true}\n").unwrap();
        drop(file);
        assert_eq!(read_jsonl_file(&path).unwrap(), vec![json!({"tail":true})]);
    }

    #[test]
    fn jsonc_matches_official_bun_tolerant_parse_and_local_array_edits() {
        // Actual unchanged CC wrappers + local Microsoft jsonc-parser 3.3.1,
        // Bun oracle: research/terminal-setup-0913/jsonc/oracle.ts.
        // Lone-surrogate Value projection is tested explicitly below; editing
        // retains its UTF-16 source units and has exact original expectations.
        let items: Vec<Value> = serde_json::from_str(r####"[{"key":"shift+enter","command":"workbench.action.terminal.sendSequence","args":{"text":"\u001b\r"},"when":"terminalFocus"},null,[1,{"z":"\ud83d\ude00"}],{"2":1e-07,"10":1,"z":0,"a":100000000000000000000,"b":1e+21,"c":18446744073709552000},1.25]"####).unwrap();
        let cases: Vec<Value> = serde_json::from_str(r####"[
{"input":"","defined":true,"parsed":null,"edits":["[\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    null\n]","[\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    1.25\n]"]},
{"input":" ","defined":false,"parsed":null,"edits":["[\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    null\n]","[\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    1.25\n]"]},
{"input":"null","defined":true,"parsed":null,"edits":["[\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    null\n]","[\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    1.25\n]"]},
{"input":"hello","defined":false,"parsed":null,"edits":["[\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    null\n]","[\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    1.25\n]"]},
{"input":"{bad","defined":true,"parsed":{},"edits":["[\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    null\n]","[\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    1.25\n]"]},
{"input":"[1,,2]","defined":true,"parsed":[1,2],"edits":["[\n    1,\n    ,\n    2,\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    1,\n    ,\n    2,\n    null\n]","[\n    1,\n    ,\n    2,\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    1,\n    ,\n    2,\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    1,\n    ,\n    2,\n    1.25\n]"]},
{"input":"[1,2","defined":true,"parsed":[1,2],"edits":["[\n    1,\n    2,\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }","[\n    1,\n    2,\n    null","[\n    1,\n    2,\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]","[\n    1,\n    2,\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }","[\n    1,\n    2,\n    1.25"]},
{"input":"[","defined":true,"parsed":[],"edits":["[\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }","[\n    null","[\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]","[\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }","[\n    1.25"]},
{"input":"[]","defined":true,"parsed":[],"edits":["[\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    null\n]","[\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    1.25\n]"]},
{"input":"[ ]","defined":true,"parsed":[],"edits":["[\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    null\n]","[\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    1.25\n]"]},
{"input":"[1]","defined":true,"parsed":[1],"edits":["[\n    1,\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    1,\n    null\n]","[\n    1,\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    1,\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    1,\n    1.25\n]"]},
{"input":"[1,2]","defined":true,"parsed":[1,2],"edits":["[\n    1,\n    2,\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    1,\n    2,\n    null\n]","[\n    1,\n    2,\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    1,\n    2,\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    1,\n    2,\n    1.25\n]"]},
{"input":"[1,]","defined":true,"parsed":[1],"edits":["[\n    1,\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    },\n]","[\n    1,\n    null,\n]","[\n    1,\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ],\n]","[\n    1,\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    },\n]","[\n    1,\n    1.25,\n]"]},
{"input":"[,]","defined":true,"parsed":[],"edits":["[\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    },\n]","[\n    null,\n]","[\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ],\n]","[\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    },\n]","[\n    1.25,\n]"]},
{"input":"[,,]","defined":true,"parsed":[],"edits":["[\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    },\n    ,\n]","[\n    null,\n    ,\n]","[\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ],\n    ,\n]","[\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    },\n    ,\n]","[\n    1.25,\n    ,\n]"]},
{"input":"// lead\n[1, /*tail*/ 2]\n","defined":true,"parsed":[1,2],"edits":["// lead\n[\n    1, /*tail*/\n    2,\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]\n","// lead\n[\n    1, /*tail*/\n    2,\n    null\n]\n","// lead\n[\n    1, /*tail*/\n    2,\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]\n","// lead\n[\n    1, /*tail*/\n    2,\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]\n","// lead\n[\n    1, /*tail*/\n    2,\n    1.25\n]\n"]},
{"input":"// empty\n[/*inside*/]\n","defined":true,"parsed":[],"edits":["// empty\n[\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    } /*inside*/\n]\n","// empty\n[\n    null /*inside*/\n]\n","// empty\n[\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ] /*inside*/\n]\n","// empty\n[\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    } /*inside*/\n]\n","// empty\n[\n    1.25 /*inside*/\n]\n"]},
{"input":"[1] // last\n","defined":true,"parsed":[1],"edits":["[\n    1,\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n] // last\n","[\n    1,\n    null\n] // last\n","[\n    1,\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n] // last\n","[\n    1,\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n] // last\n","[\n    1,\n    1.25\n] // last\n"]},
{"input":"[1, /* trailing */]\n","defined":true,"parsed":[1],"edits":["[\n    1,\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }, /* trailing */\n]\n","[\n    1,\n    null, /* trailing */\n]\n","[\n    1,\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ], /* trailing */\n]\n","[\n    1,\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }, /* trailing */\n]\n","[\n    1,\n    1.25, /* trailing */\n]\n"]},
{"input":"[\n  {\"key\":\"one\"},\n  // tail\n]\n","defined":true,"parsed":[{"key":"one"}],"edits":["[\n{\n    \"key\": \"one\"\n},\n{\n    \"key\": \"shift+enter\",\n    \"command\": \"workbench.action.terminal.sendSequence\",\n    \"args\": {\n        \"text\": \"\\u001b\\r\"\n    },\n    \"when\": \"terminalFocus\"\n},\n  // tail\n]\n","[\n{\n    \"key\": \"one\"\n},\nnull,\n  // tail\n]\n","[\n{\n    \"key\": \"one\"\n},\n[\n    1,\n    {\n        \"z\": \"\ud83d\ude00\"\n    }\n],\n  // tail\n]\n","[\n{\n    \"key\": \"one\"\n},\n{\n    \"2\": 1e-7,\n    \"10\": 1,\n    \"z\": 0,\n    \"a\": 100000000000000000000,\n    \"b\": 1e+21,\n    \"c\": 18446744073709552000\n},\n  // tail\n]\n","[\n{\n    \"key\": \"one\"\n},\n1.25,\n  // tail\n]\n"]},
{"input":"[\n  {\"key\":\"one\"} // tail\n]\n","defined":true,"parsed":[{"key":"one"}],"edits":["[\n{\n    \"key\": \"one\"\n},\n{\n    \"key\": \"shift+enter\",\n    \"command\": \"workbench.action.terminal.sendSequence\",\n    \"args\": {\n        \"text\": \"\\u001b\\r\"\n    },\n    \"when\": \"terminalFocus\"\n} // tail\n]\n","[\n{\n    \"key\": \"one\"\n},\nnull // tail\n]\n","[\n{\n    \"key\": \"one\"\n},\n[\n    1,\n    {\n        \"z\": \"\ud83d\ude00\"\n    }\n] // tail\n]\n","[\n{\n    \"key\": \"one\"\n},\n{\n    \"2\": 1e-7,\n    \"10\": 1,\n    \"z\": 0,\n    \"a\": 100000000000000000000,\n    \"b\": 1e+21,\n    \"c\": 18446744073709552000\n} // tail\n]\n","[\n{\n    \"key\": \"one\"\n},\n1.25 // tail\n]\n"]},
{"input":"[\n  {\"key\":\"one\"}\n]\n","defined":true,"parsed":[{"key":"one"}],"edits":["[\n{\n    \"key\": \"one\"\n},\n{\n    \"key\": \"shift+enter\",\n    \"command\": \"workbench.action.terminal.sendSequence\",\n    \"args\": {\n        \"text\": \"\\u001b\\r\"\n    },\n    \"when\": \"terminalFocus\"\n}\n]\n","[\n{\n    \"key\": \"one\"\n},\nnull\n]\n","[\n{\n    \"key\": \"one\"\n},\n[\n    1,\n    {\n        \"z\": \"\ud83d\ude00\"\n    }\n]\n]\n","[\n{\n    \"key\": \"one\"\n},\n{\n    \"2\": 1e-7,\n    \"10\": 1,\n    \"z\": 0,\n    \"a\": 100000000000000000000,\n    \"b\": 1e+21,\n    \"c\": 18446744073709552000\n}\n]\n","[\n{\n    \"key\": \"one\"\n},\n1.25\n]\n"]},
{"input":"[\r\n\t{\"key\":\"one\"}\r\n]\r\n","defined":true,"parsed":[{"key":"one"}],"edits":["[\r\n    {\r\n        \"key\": \"one\"\r\n    },\r\n    {\r\n        \"key\": \"shift+enter\",\r\n        \"command\": \"workbench.action.terminal.sendSequence\",\r\n        \"args\": {\r\n            \"text\": \"\\u001b\\r\"\r\n        },\r\n        \"when\": \"terminalFocus\"\r\n    }\r\n]\r\n","[\r\n    {\r\n        \"key\": \"one\"\r\n    },\r\n    null\r\n]\r\n","[\r\n    {\r\n        \"key\": \"one\"\r\n    },\r\n    [\r\n        1,\r\n        {\r\n            \"z\": \"\ud83d\ude00\"\r\n        }\r\n    ]\r\n]\r\n","[\r\n    {\r\n        \"key\": \"one\"\r\n    },\r\n    {\r\n        \"2\": 1e-7,\r\n        \"10\": 1,\r\n        \"z\": 0,\r\n        \"a\": 100000000000000000000,\r\n        \"b\": 1e+21,\r\n        \"c\": 18446744073709552000\r\n    }\r\n]\r\n","[\r\n    {\r\n        \"key\": \"one\"\r\n    },\r\n    1.25\r\n]\r\n"]},
{"input":"\t[1,2]","defined":true,"parsed":[1,2],"edits":["    [\n        1,\n        2,\n        {\n            \"key\": \"shift+enter\",\n            \"command\": \"workbench.action.terminal.sendSequence\",\n            \"args\": {\n                \"text\": \"\\u001b\\r\"\n            },\n            \"when\": \"terminalFocus\"\n        }\n    ]","    [\n        1,\n        2,\n        null\n    ]","    [\n        1,\n        2,\n        [\n            1,\n            {\n                \"z\": \"\ud83d\ude00\"\n            }\n        ]\n    ]","    [\n        1,\n        2,\n        {\n            \"2\": 1e-7,\n            \"10\": 1,\n            \"z\": 0,\n            \"a\": 100000000000000000000,\n            \"b\": 1e+21,\n            \"c\": 18446744073709552000\n        }\n    ]","    [\n        1,\n        2,\n        1.25\n    ]"]},
{"input":"\ufeff[]","defined":true,"parsed":[],"edits":["[\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    null\n]","[\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    1.25\n]"]},
{"input":"\ufeff\ufeff[]","defined":true,"parsed":[],"edits":["\ufeff[\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","\ufeff[\n    null\n]","\ufeff[\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","\ufeff[\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","\ufeff[\n    1.25\n]"]},
{"input":"[{\"x\":\"\ud83d\ude00\u4e2d\"}, /* e */ \"\u00e9\"]","defined":true,"parsed":[{"x":"\ud83d\ude00\u4e2d"},"\u00e9"],"edits":["[\n    {\n        \"x\": \"\ud83d\ude00\u4e2d\"\n    }, /* e */\n    \"\u00e9\",\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    {\n        \"x\": \"\ud83d\ude00\u4e2d\"\n    }, /* e */\n    \"\u00e9\",\n    null\n]","[\n    {\n        \"x\": \"\ud83d\ude00\u4e2d\"\n    }, /* e */\n    \"\u00e9\",\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    {\n        \"x\": \"\ud83d\ude00\u4e2d\"\n    }, /* e */\n    \"\u00e9\",\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    {\n        \"x\": \"\ud83d\ude00\u4e2d\"\n    }, /* e */\n    \"\u00e9\",\n    1.25\n]"]},
{"input":"[true false null]","defined":true,"parsed":[true,false,null],"edits":["[\n    true false null,\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    true false null,\n    null\n]","[\n    true false null,\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    true false null,\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    true false null,\n    1.25\n]"]},
{"input":"[1e,2.]","defined":true,"parsed":[1,2],"edits":["[1e,2.,\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[1e,2.,\n    null\n]","[1e,2.,\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[1e,2.,\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[1e,2.,\n    1.25\n]"]},
{"input":"[01, -0, 1e999]","defined":true,"parsed":[0,1,0,null],"edits":["[\n    01,\n    -0,\n    1e999,\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    01,\n    -0,\n    1e999,\n    null\n]","[\n    01,\n    -0,\n    1e999,\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    01,\n    -0,\n    1e999,\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    01,\n    -0,\n    1e999,\n    1.25\n]"]},
{"input":"[{\"a\":1,broken,\"b\":2}]","defined":true,"parsed":[{"a":1,"b":2}],"edits":["[\n    {\n        \"a\": 1,broken,\n        \"b\": 2\n    },\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    {\n        \"a\": 1,broken,\n        \"b\": 2\n    },\n    null\n]","[\n    {\n        \"a\": 1,broken,\n        \"b\": 2\n    },\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    {\n        \"a\": 1,broken,\n        \"b\": 2\n    },\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    {\n        \"a\": 1,broken,\n        \"b\": 2\n    },\n    1.25\n]"]},
{"input":"[{\"a\":,\"b\":2}]","defined":true,"parsed":[{"b":2}],"edits":["[\n    {\n        \"a\": ,\n        \"b\": 2\n    },\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    {\n        \"a\": ,\n        \"b\": 2\n    },\n    null\n]","[\n    {\n        \"a\": ,\n        \"b\": 2\n    },\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    {\n        \"a\": ,\n        \"b\": 2\n    },\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    {\n        \"a\": ,\n        \"b\": 2\n    },\n    1.25\n]"]},
{"input":"[{\"a\" 1,\"b\":2}]","defined":true,"parsed":[{"b":2}],"edits":["[\n    {\n        \"a\"1,\n        \"b\": 2\n    },\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    {\n        \"a\"1,\n        \"b\": 2\n    },\n    null\n]","[\n    {\n        \"a\"1,\n        \"b\": 2\n    },\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    {\n        \"a\"1,\n        \"b\": 2\n    },\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    {\n        \"a\"1,\n        \"b\": 2\n    },\n    1.25\n]"]},
{"input":"[\"bad\\qtext\", \"\\u12gh\"]","defined":true,"parsed":["badtext","gh"],"edits":["[\"bad\\qtext\", \"\\u12gh\",\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\"bad\\qtext\", \"\\u12gh\",\n    null\n]","[\"bad\\qtext\", \"\\u12gh\",\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\"bad\\qtext\", \"\\u12gh\",\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\"bad\\qtext\", \"\\u12gh\",\n    1.25\n]"]},
{"input":"[\"line\nnext\",2]","defined":true,"parsed":["line",",2]"],"edits":["[\"line\nnext\",2],{\"key\":\"shift+enter\",\"command\":\"workbench.action.terminal.sendSequence\",\"args\":{\"text\":\"\\u001b\\r\"},\"when\":\"terminalFocus\"}","[\"line\nnext\",2],null","[\"line\nnext\",2],[1,{\"z\":\"\ud83d\ude00\"}]","[\"line\nnext\",2],{\"2\":1e-7,\"10\":1,\"z\":0,\"a\":100000000000000000000,\"b\":1e+21,\"c\":18446744073709552000}","[\"line\nnext\",2],1.25"]},
{"input":"[\"unterminated]","defined":true,"parsed":["unterminated]"],"edits":["[\n    \"unterminated],{\"key\":\"shift+enter\",\"command\":\"workbench.action.terminal.sendSequence\",\"args\":{\"text\":\"\\u001b\\r\"},\"when\":\"terminalFocus\"}","[\"unterminated],null","[\n    \"unterminated],[1,{\"z\":\"\ud83d\ude00\"}]","[\n    \"unterminated],{\"2\":1e-7,\"10\":1,\"z\":0,\"a\":100000000000000000000,\"b\":1e+21,\"c\":18446744073709552000}","[\"unterminated],1.25"]},
{"input":"/*","defined":false,"parsed":null,"edits":["[\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    null\n]","[\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    1.25\n]"]},
{"input":"[/*","defined":true,"parsed":[],"edits":["[\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }/*","[\n    null/*","[\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]/*","[\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }/*","[\n    1.25/*"]},
{"input":"[1 /*","defined":true,"parsed":[1],"edits":["[\n    1,\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    } /*","[\n    1,\n    null /*","[\n    1,\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ] /*","[\n    1,\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    } /*","[\n    1,\n    1.25 /*"]},
{"input":"[1, /*","defined":true,"parsed":[1],"edits":["[\n    1,\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }, /*","[\n    1,\n    null, /*","[\n    1,\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ], /*","[\n    1,\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }, /*","[\n    1,\n    1.25, /*"]},
{"input":"[{\"x\":1}","defined":true,"parsed":[{"x":1}],"edits":["[\n    {\n        \"x\": 1\n    },\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }","[\n    {\n        \"x\": 1\n    },\n    null","[\n    {\n        \"x\": 1\n    },\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]","[\n    {\n        \"x\": 1\n    },\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }","[\n    {\n        \"x\": 1\n    },\n    1.25"]},
{"input":"[{\"__proto__\":{\"x\":1},\"a\":2}]","defined":true,"parsed":[{"a":2}],"edits":["[\n    {\n        \"__proto__\": {\n            \"x\": 1\n        },\n        \"a\": 2\n    },\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    {\n        \"__proto__\": {\n            \"x\": 1\n        },\n        \"a\": 2\n    },\n    null\n]","[\n    {\n        \"__proto__\": {\n            \"x\": 1\n        },\n        \"a\": 2\n    },\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    {\n        \"__proto__\": {\n            \"x\": 1\n        },\n        \"a\": 2\n    },\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    {\n        \"__proto__\": {\n            \"x\": 1\n        },\n        \"a\": 2\n    },\n    1.25\n]"]},
{"input":"[\"\\ud83d\\ude00\"]","defined":true,"parsed":["\ud83d\ude00"],"edits":["[\n    \"\\ud83d\\ude00\",\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    \"\\ud83d\\ude00\",\n    null\n]","[\n    \"\\ud83d\\ude00\",\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    \"\\ud83d\\ude00\",\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    \"\\ud83d\\ude00\",\n    1.25\n]"]},
{"input":"[1] trailing words","defined":true,"parsed":[1],"edits":["[\n    1,\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n] trailing words","[\n    1,\n    null\n] trailing words","[\n    1,\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n] trailing words","[\n    1,\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n] trailing words","[\n    1,\n    1.25\n] trailing words"]},
{"input":"[1]\n[2]","defined":true,"parsed":[1],"edits":["[\n    1,\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]\n[2]","[\n    1,\n    null\n]\n[2]","[\n    1,\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]\n[2]","[\n    1,\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]\n[2]","[\n    1,\n    1.25\n]\n[2]"]},
{"input":"[\n\n1\n\n]","defined":true,"parsed":[1],"edits":["[\n\n1,\n{\n    \"key\": \"shift+enter\",\n    \"command\": \"workbench.action.terminal.sendSequence\",\n    \"args\": {\n        \"text\": \"\\u001b\\r\"\n    },\n    \"when\": \"terminalFocus\"\n}\n\n]","[\n\n1,\nnull\n\n]","[\n\n1,\n[\n    1,\n    {\n        \"z\": \"\ud83d\ude00\"\n    }\n]\n\n]","[\n\n1,\n{\n    \"2\": 1e-7,\n    \"10\": 1,\n    \"z\": 0,\n    \"a\": 100000000000000000000,\n    \"b\": 1e+21,\n    \"c\": 18446744073709552000\n}\n\n]","[\n\n1,\n1.25\n\n]"]},
{"input":"[\n    // a\n    1,\n    // b\n    2\n]","defined":true,"parsed":[1,2],"edits":["[\n    // a\n    1,\n    // b\n    2,\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    // a\n    1,\n    // b\n    2,\n    null\n]","[\n    // a\n    1,\n    // b\n    2,\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    // a\n    1,\n    // b\n    2,\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    // a\n    1,\n    // b\n    2,\n    1.25\n]"]},
{"input":"[1, // last\n]","defined":true,"parsed":[1],"edits":["[\n    1,\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }, // last\n]","[\n    1,\n    null, // last\n]","[\n    1,\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ], // last\n]","[\n    1,\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }, // last\n]","[\n    1,\n    1.25, // last\n]"]},
{"input":"/*before*/ [] /*after*/","defined":true,"parsed":[],"edits":["/*before*/ [\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n] /*after*/","/*before*/ [\n    null\n] /*after*/","/*before*/ [\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n] /*after*/","/*before*/ [\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n] /*after*/","/*before*/ [\n    1.25\n] /*after*/"]},
{"input":"[ /*a*/ 1 /*b*/ , /*c*/ 2 /*d*/ ]","defined":true,"parsed":[1,2],"edits":["[ /*a*/\n    1 /*b*/, /*c*/\n    2,\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    } /*d*/\n]","[ /*a*/\n    1 /*b*/, /*c*/\n    2,\n    null /*d*/\n]","[ /*a*/\n    1 /*b*/, /*c*/\n    2,\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ] /*d*/\n]","[ /*a*/\n    1 /*b*/, /*c*/\n    2,\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    } /*d*/\n]","[ /*a*/\n    1 /*b*/, /*c*/\n    2,\n    1.25 /*d*/\n]"]},
{"input":"[{\"a\": [1, {\"b\":2}], \"c\": {}}]","defined":true,"parsed":[{"a":[1,{"b":2}],"c":{}}],"edits":["[\n    {\n        \"a\": [\n            1,\n            {\n                \"b\": 2\n            }\n        ],\n        \"c\": {}\n    },\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    {\n        \"a\": [\n            1,\n            {\n                \"b\": 2\n            }\n        ],\n        \"c\": {}\n    },\n    null\n]","[\n    {\n        \"a\": [\n            1,\n            {\n                \"b\": 2\n            }\n        ],\n        \"c\": {}\n    },\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    {\n        \"a\": [\n            1,\n            {\n                \"b\": 2\n            }\n        ],\n        \"c\": {}\n    },\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    {\n        \"a\": [\n            1,\n            {\n                \"b\": 2\n            }\n        ],\n        \"c\": {}\n    },\n    1.25\n]"]},
{"input":"[\"a\tb\"]","defined":true,"parsed":["a\tb"],"edits":["[\"a\tb\",\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\"a\tb\",\n    null\n]","[\"a\tb\",\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\"a\tb\",\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\"a\tb\",\n    1.25\n]"]},
{"input":"[1.2e-3,-2E+4]","defined":true,"parsed":[0.0012,-20000],"edits":["[\n    1.2e-3,\n    -2E+4,\n    {\n        \"key\": \"shift+enter\",\n        \"command\": \"workbench.action.terminal.sendSequence\",\n        \"args\": {\n            \"text\": \"\\u001b\\r\"\n        },\n        \"when\": \"terminalFocus\"\n    }\n]","[\n    1.2e-3,\n    -2E+4,\n    null\n]","[\n    1.2e-3,\n    -2E+4,\n    [\n        1,\n        {\n            \"z\": \"\ud83d\ude00\"\n        }\n    ]\n]","[\n    1.2e-3,\n    -2E+4,\n    {\n        \"2\": 1e-7,\n        \"10\": 1,\n        \"z\": 0,\n        \"a\": 100000000000000000000,\n        \"b\": 1e+21,\n        \"c\": 18446744073709552000\n    }\n]","[\n    1.2e-3,\n    -2E+4,\n    1.25\n]"]}
]"####).unwrap();
        for case in cases {
            let input = case["input"].as_str().unwrap();
            let expected = case["defined"]
                .as_bool()
                .unwrap()
                .then(|| case["parsed"].clone());
            assert_eq!(
                safe_parse_jsonc(input).map(|value| value.to_json()),
                expected,
                "parse {input:?}"
            );
            for (i, item) in items.iter().enumerate() {
                assert_eq!(
                    add_item_to_jsonc_array(input, item),
                    case["edits"][i].as_str().unwrap(),
                    "edit {input:?}, item {item}"
                );
            }
        }
    }

    #[test]
    fn jsonc_matches_official_utf16_edit_offsets_and_documents_value_projection() {
        // JS strings can contain lone UTF-16 surrogates, Rust String cannot.
        // This is the serde Value carrier boundary, not a tokenizer/editor loss:
        // token units and retained source escape sequences remain exact.
        for (input, expected, unit) in [
            (r#"["\ud800"]"#, "[\n    \"\\ud800\",\n    null\n]", 0xd800),
            (r#"["\udc00"]"#, "[\n    \"\\udc00\",\n    null\n]", 0xdc00),
        ] {
            let text: Vec<u16> = input.encode_utf16().collect();
            let mut scanner = JsoncScanner::new(&text);
            assert_eq!(scanner.scan().kind, 3);
            assert_eq!(scanner.scan().value, vec![unit]);
            assert_eq!(add_item_to_jsonc_array(input, &Value::Null), expected);
            assert_eq!(
                safe_parse_jsonc(input).map(|value| value.to_json()),
                Some(json!(["\u{fffd}"]))
            );
        }
        // JS Infinity has no serde JSON-number representation; its JSON
        // serialization, like the existing JSON Value carrier, is null.
        assert_eq!(
            safe_parse_jsonc("[1e999]").map(|value| value.to_json()),
            Some(json!([null]))
        );
    }

    #[test]
    fn jsonc_matches_official_overlapping_edit_logs_and_falls_back() {
        // jsonc-parser scanner.js + main.js#applyEdits: an unterminated block
        // comment ending in CRLF can put an enclosing array end past EOF.
        // CC utils/json.ts:273–276 logs the resulting exception and replaces.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _env: Vec<_> = [
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "DISABLE_ERROR_REPORTING",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
        ]
        .into_iter()
        .map(EnvVarGuard::unset)
        .collect();
        crate::utils::log::_reset_error_log_for_testing();
        assert_eq!(
            add_item_to_jsonc_array("[[1/*\r\n", &Value::Null),
            "[\n    null\n]"
        );
        let errors = crate::utils::log::get_in_memory_errors();
        assert_eq!(errors.len(), 1);
        assert!(format!("{errors:?}").contains("Overlapping edit"));
    }
    #[test]
    fn jsonc_matches_official_inherited_find_and_formatter_boundaries() {
        // Actual CC parser + terminalSetup `.find` callback + array editor,
        // oracle.ts and independent jsonc-review-a oracle; not Array.isArray.
        let cases: Vec<Value> = serde_json::from_str(r####"[{"input":"[1e999]","consumer":{"found":false},"edited":"[\n    1e999,\n    null\n]","parsed":[null]},{"input":"[{\"__proto__\":{\"key\":\"shift+enter\",\"command\":\"workbench.action.terminal.sendSequence\",\"when\":\"terminalFocus\"}}]","consumer":{"found":true},"edited":"[\n    {\n        \"__proto__\": {\n            \"key\": \"shift+enter\",\n            \"command\": \"workbench.action.terminal.sendSequence\",\n            \"when\": \"terminalFocus\"\n        }\n    },\n    null\n]","parsed":[{}]},{"input":"[{\"__proto__\":null,\"__proto__\":{\"key\":\"shift+enter\",\"command\":\"workbench.action.terminal.sendSequence\",\"when\":\"terminalFocus\"}}]","consumer":{"found":false},"edited":"[\n    {\n        \"__proto__\": null,\n        \"__proto__\": {\n            \"key\": \"shift+enter\",\n            \"command\": \"workbench.action.terminal.sendSequence\",\n            \"when\": \"terminalFocus\"\n        }\n    },\n    null\n]","parsed":[{"__proto__":{"key":"shift+enter","command":"workbench.action.terminal.sendSequence","when":"terminalFocus"}}]},{"input":"[{\"__proto__\":{\"__proto__\":null},\"__proto__\":{\"key\":\"shift+enter\",\"command\":\"workbench.action.terminal.sendSequence\",\"when\":\"terminalFocus\"}}]","consumer":{"found":false},"edited":"[\n    {\n        \"__proto__\": {\n            \"__proto__\": null\n        },\n        \"__proto__\": {\n            \"key\": \"shift+enter\",\n            \"command\": \"workbench.action.terminal.sendSequence\",\n            \"when\": \"terminalFocus\"\n        }\n    },\n    null\n]","parsed":[{"__proto__":{"key":"shift+enter","command":"workbench.action.terminal.sendSequence","when":"terminalFocus"}}]},{"input":"{\"__proto__\":[],\"length\":0}","consumer":{"found":false},"edited":"[\n    null\n]","parsed":{"length":0}},{"input":"{\"__proto__\":[],\"length\":\"1\",\"0\":{\"key\":\"shift+enter\",\"command\":\"workbench.action.terminal.sendSequence\",\"when\":\"terminalFocus\"}}","consumer":{"found":true},"edited":"[\n    null\n]","parsed":{"0":{"key":"shift+enter","command":"workbench.action.terminal.sendSequence","when":"terminalFocus"},"length":"1"}},{"input":"{\"__proto__\":[{\"key\":\"shift+enter\",\"command\":\"workbench.action.terminal.sendSequence\",\"when\":\"terminalFocus\"}]}","consumer":{"found":true},"edited":"[\n    null\n]","parsed":{}},{"input":"{\"__proto__\":[],\"length\":1}","consumer":{"error":{"name":"TypeError","message":"undefined is not an object (evaluating 'b.key')"}},"edited":"[\n    null\n]","parsed":{"length":1}},{"input":"{\"__proto__\":[],\"find\":null,\"length\":0}","consumer":{"error":{"name":"TypeError","message":"(parsed || []).find is not a function. (In '(parsed || []).find((b) => b.key === \"shift+enter\" && b.command === \"workbench.action.terminal.sendSequence\" && b.when === \"terminalFocus\")', '(parsed || []).find' is null)"}},"edited":"[\n    null\n]","parsed":{"find":null,"length":0}},{"input":"                                                                                                                                                                                                []","consumer":{"found":false},"edited":"                                                                                                                                                                                                [\n                                                                                                                                                                                                    null\n                                                                                                                                                                                                ]","parsed":[]},{"input":"                                                                                                                                                                                                    []","consumer":{"found":false},"edited":"[\n    null\n]","parsed":[]},{"input":"                                                                                                                                                                                                        []","consumer":{"found":false},"edited":"[\n    null\n]","parsed":[]},{"input":"                                                                                                                                                                                                            []","consumer":{"found":false},"edited":"                                                                                                                                                                                                            [\n                                                                                                                                                                                                                null\n                                                                                                                                                                                                            ]","parsed":[]}]"####).unwrap();
        for case in cases {
            let input = case["input"].as_str().unwrap();
            let parsed = safe_parse_jsonc(input).unwrap();
            assert_eq!(parsed.to_json(), case["parsed"], "parse {input}");
            let found = (|| -> Result<bool, ()> {
                for index in 0..parsed.array_find_length()? {
                    let binding = parsed.array_find_item(index).ok_or(())?;
                    if binding.is_null() {
                        return Err(());
                    }
                    if binding.get_property("key").and_then(JsoncValue::as_str)
                        == Some("shift+enter")
                        && binding.get_property("command").and_then(JsoncValue::as_str)
                            == Some("workbench.action.terminal.sendSequence")
                        && binding.get_property("when").and_then(JsoncValue::as_str)
                            == Some("terminalFocus")
                    {
                        return Ok(true);
                    }
                }
                Ok(false)
            })();
            if let Some(expected) = case["consumer"]["found"].as_bool() {
                assert_eq!(found, Ok(expected), "find {input}");
            } else {
                assert!(found.is_err(), "find must throw {input}");
            }
            assert_eq!(
                add_item_to_jsonc_array(input, &Value::Null),
                case["edited"].as_str().unwrap(),
                "edit {input}"
            );
        }
        // Review A: inherited Array.prototype.toString/join participates in
        // ToLength conversion; the receiver need not be an array itself.
        let nested_length = safe_parse_jsonc(
            r#"{"__proto__":[],"length":{"__proto__":[1]},"0":{"key":"shift+enter"}}"#,
        )
        .unwrap();
        assert_eq!(nested_length.array_find_length(), Ok(1));
        assert_eq!(
            nested_length
                .array_find_item(0)
                .unwrap()
                .get_property("key")
                .and_then(JsoncValue::as_str),
            Some("shift+enter")
        );
        let nonfinite = safe_parse_jsonc("[1e999]").unwrap();
        assert!(!nonfinite.array_find_item(0).unwrap().is_null());
        assert!(
            safe_parse_jsonc("[null]")
                .unwrap()
                .array_find_item(0)
                .unwrap()
                .is_null()
        );
    }
}
