//! @nyan: in-process BM25 text index (docs/nyan/retrieval.md 4.3/4.5). One
//! Tantivy directory per generation, under the worktree's retrieval storage —
//! never inside the worktree. The analyzer splits identifiers
//! (`autoCompactIfNeeded` → `auto compact if needed`) and natural language the
//! same way, so a snake/camel query and a prose query meet on the same terms.
//!
//! The schema/analyzer names are part of the index generation: changing either
//! produces a new generation and therefore a new directory.

use std::path::Path;

use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value,
};
use tantivy::tokenizer::{TextAnalyzer, Token, TokenStream, Tokenizer};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, Term, doc};

pub const ANALYZER_NAME: &str = "nyan_code_v1";
pub const SCHEMA_VERSION: &str = "nyan-text-v3";
const WRITER_MEMORY_BYTES: usize = 50_000_000;
pub const SEARCH_LIMIT: usize = 300;

#[derive(Clone, Debug, Default)]
pub struct TextHit {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub symbol: Option<String>,
    /// Raw file hash captured at index time. Inspect compares it against the
    /// current file, so a hit from a stale segment is dropped rather than
    /// re-labeled with the fresh hash.
    pub file_hash_raw: String,
    pub score: f64,
}

#[derive(Clone, Debug)]
pub struct ChunkDocument {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub symbol: Option<String>,
    pub file_hash_raw: String,
    pub body: String,
}

pub struct TextIndex {
    index: Index,
    reader: IndexReader,
    path_field: Field,
    start_field: Field,
    end_field: Field,
    symbol_field: Field,
    hash_field: Field,
    body_field: Field,
}

impl Clone for TextIndex {
    fn clone(&self) -> Self {
        Self {
            index: self.index.clone(),
            reader: self.reader.clone(),
            path_field: self.path_field,
            start_field: self.start_field,
            end_field: self.end_field,
            symbol_field: self.symbol_field,
            hash_field: self.hash_field,
            body_field: self.body_field,
        }
    }
}

/// Identifier/natural-language tokenizer. Emits `[a-z0-9]+` words after
/// splitting snake_case, kebab-case, camelCase and PascalCase, with byte
/// offsets so Tantivy positions stay meaningful.
#[derive(Clone, Default)]
pub struct CodeTokenizer;

pub struct CodeTokenStream {
    tokens: Vec<Token>,
    cursor: usize,
}

impl Tokenizer for CodeTokenizer {
    type TokenStream<'a> = CodeTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        CodeTokenStream {
            tokens: tokenize_with_offsets(text)
                .into_iter()
                .enumerate()
                .map(|(position, (offset_from, offset_to, token))| Token {
                    offset_from,
                    offset_to,
                    position,
                    text: token,
                    position_length: 1,
                })
                .collect(),
            cursor: 0,
        }
    }
}

impl TokenStream for CodeTokenStream {
    fn advance(&mut self) -> bool {
        self.cursor += 1;
        self.cursor <= self.tokens.len()
    }

    fn token(&self) -> &Token {
        &self.tokens[self.cursor - 1]
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.tokens[self.cursor - 1]
    }
}

/// Split `text` into lowered words, preserving byte offsets. Non-ASCII
/// characters separate tokens; ICU word segmentation is not needed because
/// code identifiers and queries are dominated by ASCII.
pub fn tokenize_with_offsets(text: &str) -> Vec<(usize, usize, String)> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut word_start = None;
    for (offset, character) in text.char_indices() {
        let is_word = character.is_ascii_alphanumeric() || character == '_' || character == '-';
        if is_word {
            word_start.get_or_insert(offset);
            continue;
        }
        if let Some(start) = word_start.take() {
            push_identifier(bytes, start, offset, &mut tokens);
        }
    }
    if let Some(start) = word_start {
        push_identifier(bytes, start, bytes.len(), &mut tokens);
    }
    tokens
}

fn push_identifier(
    bytes: &[u8],
    start: usize,
    end: usize,
    tokens: &mut Vec<(usize, usize, String)>,
) {
    let mut word_start = start;
    let mut previous: Option<u8> = None;
    for index in start..end {
        let byte = bytes[index];
        let separator = byte == b'_' || byte == b'-';
        // Split on an upper-case letter that follows a lower-case letter or a
        // digit (`autoCompact`, `sha256Hash`). Runs of capitals (`HTTPClient`)
        // split before the last capital so `HTTP` + `Client` survive.
        let camel = byte.is_ascii_uppercase()
            && previous
                .is_some_and(|previous| previous.is_ascii_lowercase() || previous.is_ascii_digit());
        let acronym_split = byte.is_ascii_uppercase()
            && previous.is_some_and(|previous| previous.is_ascii_uppercase())
            && bytes.get(index + 1).is_some_and(u8::is_ascii_lowercase);
        if separator || camel {
            if word_start < index {
                push_word(bytes, word_start, index, tokens);
            }
            word_start = index + usize::from(separator);
        } else if acronym_split {
            if word_start < index {
                push_word(bytes, word_start, index, tokens);
            }
            word_start = index;
        }
        previous = Some(byte);
    }
    if word_start < end {
        push_word(bytes, word_start, end, tokens);
    }
}

fn push_word(bytes: &[u8], start: usize, end: usize, tokens: &mut Vec<(usize, usize, String)>) {
    let word = &bytes[start..end];
    if word.is_empty() {
        return;
    }
    let lowered = String::from_utf8_lossy(word).to_ascii_lowercase();
    if lowered
        .chars()
        .all(|character| !character.is_ascii_alphanumeric())
    {
        return;
    }
    tokens.push((start, end, lowered));
}

impl TextIndex {
    /// Open (or create) the index directory for one generation. A directory
    /// whose schema is unreadable or different is recreated, which is safe
    /// because the index is a rebuildable derivative.
    pub fn open(directory: &Path) -> Result<Self, String> {
        let (schema, path_field, start_field, end_field, symbol_field, hash_field, body_field) =
            schema();
        let index = match Index::open_in_dir(directory) {
            Ok(index) if index.schema() == schema => index,
            _ => {
                if directory.exists() {
                    std::fs::remove_dir_all(directory).map_err(|error| error.to_string())?;
                }
                std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
                Index::create_in_dir(directory, schema).map_err(|error| error.to_string())?
            }
        };
        register_analyzer(&index);
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(|error: tantivy::TantivyError| error.to_string())?;
        Ok(Self {
            index,
            reader,
            path_field,
            start_field,
            end_field,
            symbol_field,
            hash_field,
            body_field,
        })
    }

    /// Open a reader-only handle on an existing index directory. A missing or
    /// schema-mismatched directory is `None`, not an empty index.
    pub fn open_existing(directory: &Path) -> Option<Self> {
        if !directory.is_dir() {
            return None;
        }
        let (schema, path_field, start_field, end_field, symbol_field, hash_field, body_field) =
            schema();
        let index = Index::open_in_dir(directory).ok()?;
        if index.schema() != schema {
            return None;
        }
        register_analyzer(&index);
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .ok()?;
        Some(Self {
            index,
            reader,
            path_field,
            start_field,
            end_field,
            symbol_field,
            hash_field,
            body_field,
        })
    }

    /// Replace every chunk of one file. The caller commits once per batch.
    pub fn replace_file(writer: &mut IndexWriter, fields: &TextFields, document: &ChunkDocument) {
        let _ = writer.delete_term(Term::from_field_text(fields.path, &document.path));
        writer
            .add_document(doc!(
                fields.path => document.path.clone(),
                fields.start => document.start_line as u64,
                fields.end => document.end_line as u64,
                fields.symbol => document.symbol.clone().unwrap_or_default(),
                fields.hash => document.file_hash_raw.clone(),
                fields.body => document.body.clone(),
            ))
            .expect("tantivy document fields match the schema");
    }

    pub fn writer(&self) -> Result<IndexWriter, String> {
        self.index
            .writer_with_num_threads(1, WRITER_MEMORY_BYTES)
            .map_err(|error| error.to_string())
    }

    pub fn fields(&self) -> TextFields {
        TextFields {
            path: self.path_field,
            start: self.start_field,
            end: self.end_field,
            symbol: self.symbol_field,
            hash: self.hash_field,
            body: self.body_field,
        }
    }

    pub fn delete_file(writer: &mut IndexWriter, fields: &TextFields, path: &str) {
        let _ = writer.delete_term(Term::from_field_text(fields.path, path));
    }

    pub fn reload(&self) -> Result<(), String> {
        self.reader.reload().map_err(|error| error.to_string())
    }

    /// BM25 search. Ties are resolved by path bytes and start line so equal
    /// scores never depend on segment order.
    pub fn search(&self, query: &str, max_results: usize) -> Result<Vec<TextHit>, String> {
        self.reader.reload().map_err(|error| error.to_string())?;
        let searcher = self.reader.searcher();
        let clauses = tokenize_with_offsets(query)
            .into_iter()
            .map(|(_, _, text)| {
                Box::new(TermQuery::new(
                    Term::from_field_text(self.body_field, &text),
                    IndexRecordOption::WithFreqs,
                )) as Box<dyn tantivy::query::Query>
            })
            .collect::<Vec<_>>();
        if clauses.is_empty() {
            return Ok(Vec::new());
        }
        let parsed = BooleanQuery::new(
            clauses
                .into_iter()
                .map(|clause| (Occur::Should, clause))
                .collect(),
        );
        let found = searcher
            .search(&parsed, &TopDocs::with_limit(SEARCH_LIMIT).order_by_score())
            .map_err(|error| error.to_string())?;
        let mut hits = Vec::new();
        for (score, address) in found {
            let document: tantivy::TantivyDocument =
                searcher.doc(address).map_err(|error| error.to_string())?;
            let path = document
                .get_first(self.path_field)
                .and_then(|value| value.as_str().map(str::to_owned));
            let start = document
                .get_first(self.start_field)
                .and_then(|value| value.as_u64());
            let end = document
                .get_first(self.end_field)
                .and_then(|value| value.as_u64());
            let symbol = document
                .get_first(self.symbol_field)
                .and_then(|value| value.as_str().map(str::to_owned))
                .filter(|value| !value.is_empty());
            let file_hash_raw = document
                .get_first(self.hash_field)
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default();
            let (Some(path), Some(start), Some(end)) = (path, start, end) else {
                continue;
            };
            hits.push(TextHit {
                path,
                start_line: start as usize,
                end_line: end as usize,
                symbol,
                file_hash_raw,
                score: f64::from(score),
            });
        }
        hits.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.path.as_bytes().cmp(right.path.as_bytes()))
                .then_with(|| left.start_line.cmp(&right.start_line))
                .then_with(|| left.end_line.cmp(&right.end_line))
        });
        hits.dedup_by(|left, right| left.path == right.path && left.start_line == right.start_line);
        hits.truncate(max_results);
        Ok(hits)
    }
}

#[derive(Clone, Copy)]
pub struct TextFields {
    pub path: Field,
    pub start: Field,
    pub end: Field,
    pub symbol: Field,
    pub hash: Field,
    pub body: Field,
}

fn schema() -> (Schema, Field, Field, Field, Field, Field, Field) {
    let body_indexing = TextFieldIndexing::default()
        .set_tokenizer(ANALYZER_NAME)
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);
    let mut builder = Schema::builder();
    let path = builder.add_text_field("path", STRING | STORED);
    let start = builder.add_u64_field("start", tantivy::schema::INDEXED | STORED);
    let end = builder.add_u64_field("end", STORED);
    let symbol = builder.add_text_field("symbol", STRING | STORED);
    let hash = builder.add_text_field("fileHash", STRING | STORED);
    let body = builder.add_text_field(
        "body",
        TextOptions::default().set_indexing_options(body_indexing),
    );
    let schema = builder.build();
    (schema, path, start, end, symbol, hash, body)
}

fn register_analyzer(index: &Index) {
    let tokenizer = TextAnalyzer::builder(CodeTokenizer).build();
    index.tokenizers().register(ANALYZER_NAME, tokenizer);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_splits_snake_camel_and_acronyms() {
        let tokens = |text: &str| {
            tokenize_with_offsets(text)
                .into_iter()
                .map(|(_, _, token)| token)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            tokens("autoCompactIfNeeded"),
            vec!["auto", "compact", "if", "needed"]
        );
        assert_eq!(tokens("HTTPClient"), vec!["http", "client"]);
        assert_eq!(tokens("parse_config-file"), vec!["parse", "config", "file"]);
        assert_eq!(tokens("查找 CodeSearch"), vec!["code", "search"]);
    }

    #[test]
    fn bm25_search_is_deterministic_and_splits_identifiers() {
        let root =
            std::env::temp_dir().join(format!("nyan-text-{}", uuid::Uuid::new_v4().simple()));
        let index = TextIndex::open(&root).unwrap();
        let fields = index.fields();
        let mut writer = index.writer().unwrap();
        for (path, start, body) in [
            ("a.rs", 1, "fn auto_compact_if_needed() {}"),
            ("b.rs", 10, "fn auto_compact_other() {}"),
            ("c.rs", 20, "unrelated"),
        ] {
            TextIndex::replace_file(
                &mut writer,
                &fields,
                &ChunkDocument {
                    path: path.to_string(),
                    start_line: start,
                    end_line: start + 3,
                    symbol: None,
                    file_hash_raw: format!("hash-{path}"),
                    body: body.to_string(),
                },
            );
        }
        writer.commit().unwrap();
        let hits = index.search("autoCompactIfNeeded", 10).unwrap();
        assert_eq!(hits[0].path, "a.rs");
        assert!(hits.iter().all(|hit| hit.path != "c.rs"));
        let natural = index.search("auto compact other", 10).unwrap();
        assert_eq!(natural[0].path, "b.rs");
        // Same query twice: same order.
        let again = index.search("autoCompactIfNeeded", 10).unwrap();
        assert_eq!(
            hits.iter()
                .map(|hit| (&hit.path, hit.start_line))
                .collect::<Vec<_>>(),
            again
                .iter()
                .map(|hit| (&hit.path, hit.start_line))
                .collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn replace_file_removes_old_chunks() {
        let root =
            std::env::temp_dir().join(format!("nyan-text-{}", uuid::Uuid::new_v4().simple()));
        let index = TextIndex::open(&root).unwrap();
        let fields = index.fields();
        let mut writer = index.writer().unwrap();
        TextIndex::replace_file(
            &mut writer,
            &fields,
            &ChunkDocument {
                path: "a.rs".into(),
                start_line: 1,
                end_line: 2,
                symbol: None,
                file_hash_raw: "hash-old".into(),
                body: "oldname".into(),
            },
        );
        writer.commit().unwrap();
        drop(writer);
        let mut writer = index.writer().unwrap();
        TextIndex::replace_file(
            &mut writer,
            &fields,
            &ChunkDocument {
                path: "a.rs".into(),
                start_line: 5,
                end_line: 6,
                symbol: None,
                file_hash_raw: "hash-new".into(),
                body: "newname".into(),
            },
        );
        writer.commit().unwrap();
        drop(writer);
        assert!(index.search("oldname", 10).unwrap().is_empty());
        let hits = index.search("newname", 10).unwrap();
        assert_eq!((hits[0].path.as_str(), hits[0].start_line), ("a.rs", 5));
        assert_eq!(hits[0].file_hash_raw, "hash-new");
        let _ = std::fs::remove_dir_all(root);
    }
}
