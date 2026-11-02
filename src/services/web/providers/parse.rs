//! Response parsing for the seven providers (external-search 2.3, 3.3.1).
//!
//! @nyan: every provider has its own envelope and snippet field; this module is
//! the only place that knows them. Snippets are capped at 500 chars here (the
//! rerank input size); the tool renders the first 300.

use serde_json::Value;

use super::{Hit, ProviderError, ProviderKind, ProviderPage};

const SNIPPET_CHARS: usize = 500;

pub(super) fn parse_hits(kind: ProviderKind, value: &Value) -> Result<Vec<Hit>, ProviderError> {
    let items = result_items(kind, value).ok_or(ProviderError::Invalid)?;
    let mut hits = Vec::new();
    for item in items {
        let Some(url) = item
            .get("url")
            .and_then(Value::as_str)
            .filter(|url| !url.is_empty())
        else {
            continue;
        };
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .filter(|title| !title.trim().is_empty())
            .unwrap_or(url)
            .to_string();
        hits.push(Hit {
            title,
            url: url.to_string(),
            snippet: snippet(kind, item),
            published_at: published_at(kind, item),
            provider_rank: hits.len() + 1,
        });
    }
    Ok(hits)
}

fn result_items(kind: ProviderKind, value: &Value) -> Option<&Vec<Value>> {
    match kind {
        ProviderKind::Brave => value.pointer("/web/results")?.as_array(),
        // v2 nests web hits under `data.web`; older shapes put the list in `data`.
        ProviderKind::Firecrawl => {
            let data = value.get("data")?;
            data.as_array().or_else(|| data.get("web")?.as_array())
        }
        // AnySearch wraps the payload in `{code, message, data: {results}}`
        // (verified live 2026-10-30); accept the unwrapped shape too.
        ProviderKind::AnySearch => value
            .pointer("/data/results")
            .or_else(|| value.get("results"))?
            .as_array(),
        _ => value.get("results")?.as_array(),
    }
}

fn snippet(kind: ProviderKind, item: &Value) -> String {
    let text = match kind {
        ProviderKind::AnySearch => first_text(item, &["snippet", "content"]),
        ProviderKind::Brave => {
            let mut parts = vec![first_text(item, &["description"])];
            if let Some(extra) = item.get("extra_snippets").and_then(Value::as_array) {
                parts.extend(extra.iter().filter_map(Value::as_str).map(str::to_string));
            }
            parts.retain(|part| !part.is_empty());
            parts.join(" ")
        }
        ProviderKind::DeepSeek => first_text(item, &["snippet", "summary"]),
        ProviderKind::Exa => first_text(item, &["highlights", "summary", "text"]),
        ProviderKind::Firecrawl => first_text(item, &["description", "snippet"]),
        ProviderKind::Tavily | ProviderKind::Searxng => first_text(item, &["content", "snippet"]),
    };
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(SNIPPET_CHARS)
        .collect()
}

fn first_text(item: &Value, keys: &[&str]) -> String {
    keys.iter()
        .filter_map(|key| item.get(*key))
        .map(value_text)
        .find(|text| !text.trim().is_empty())
        .unwrap_or_default()
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(values) => values
            .iter()
            .map(value_text)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(" … "),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn published_at(kind: ProviderKind, item: &Value) -> Option<String> {
    let keys: &[&str] = match kind {
        ProviderKind::Brave => &["page_age", "age"],
        ProviderKind::DeepSeek => &["date"],
        ProviderKind::Exa => &["publishedDate"],
        ProviderKind::Tavily => &["published_date"],
        ProviderKind::Searxng => &["publishedDate"],
        ProviderKind::AnySearch => &["published_date", "date"],
        ProviderKind::Firecrawl => &["date"],
    };
    keys.iter()
        .filter_map(|key| item.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|date| !date.is_empty())
        .map(str::to_string)
}

pub(super) fn parse_page(
    kind: ProviderKind,
    url: &str,
    value: Value,
) -> Result<ProviderPage, ProviderError> {
    let page = match kind {
        ProviderKind::Firecrawl => firecrawl_page(url, &value)?,
        ProviderKind::Exa => exa_page(url, &value)?,
        ProviderKind::Tavily => tavily_page(url, &value)?,
        ProviderKind::AnySearch => any_page(url, &value, "anysearch")?,
        ProviderKind::DeepSeek => any_page(url, &value, "deepseek")?,
        ProviderKind::Brave | ProviderKind::Searxng => return Err(ProviderError::Unsupported),
    };
    if page.content.trim().is_empty() {
        return Err(ProviderError::Invalid);
    }
    Ok(page)
}

fn page(
    url: &str,
    content: &str,
    title: Option<&str>,
    content_type: &str,
    format: &str,
) -> ProviderPage {
    ProviderPage {
        url: url.to_string(),
        final_url: url.to_string(),
        title: title.map(str::to_string),
        content: content.to_string(),
        status: 200,
        content_type: content_type.to_string(),
        format: format.to_string(),
        freshness: Some("unknown".to_string()),
    }
}

fn str_at<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer).and_then(Value::as_str)
}

/// Firecrawl reports status, content type, source URL and cache state.
fn firecrawl_page(url: &str, value: &Value) -> Result<ProviderPage, ProviderError> {
    let data = value.get("data").unwrap_or(value);
    let content = str_at(data, "/markdown")
        .or_else(|| str_at(data, "/content"))
        .ok_or(ProviderError::Invalid)?;
    let mut result = page(
        url,
        content,
        str_at(data, "/metadata/title"),
        "text/markdown",
        "markdown",
    );
    if let Some(status) = data
        .pointer("/metadata/statusCode")
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
    {
        result.status = status;
    }
    if let Some(content_type) = str_at(data, "/metadata/contentType") {
        result.content_type = content_type
            .split(';')
            .next()
            .unwrap_or(content_type)
            .trim()
            .to_ascii_lowercase();
    }
    if let Some(source) = str_at(data, "/metadata/sourceURL").filter(|source| is_http_url(source)) {
        result.final_url = source.to_string();
    }
    result.freshness = Some(match str_at(data, "/metadata/cachedAt") {
        Some(cached_at) => format!("cached:{cached_at}"),
        None => "live".to_string(),
    });
    Ok(result)
}

/// Exa returns plain text; `statuses[].source` says whether it was cached.
fn exa_page(url: &str, value: &Value) -> Result<ProviderPage, ProviderError> {
    let item = value
        .get("results")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .ok_or(ProviderError::Invalid)?;
    let content = str_at(item, "/text")
        .or_else(|| str_at(item, "/content"))
        .ok_or(ProviderError::Invalid)?;
    let mut result = page(url, content, str_at(item, "/title"), "text/plain", "text");
    let cached = value
        .get("statuses")
        .and_then(Value::as_array)
        .and_then(|statuses| statuses.first())
        .and_then(|status| str_at(status, "/source"))
        .is_some_and(|source| source == "cached");
    result.freshness = Some(if cached { "cached" } else { "live" }.to_string());
    Ok(result)
}

fn tavily_page(url: &str, value: &Value) -> Result<ProviderPage, ProviderError> {
    let item = value
        .get("results")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .ok_or(ProviderError::Invalid)?;
    let content = str_at(item, "/raw_content")
        .or_else(|| str_at(item, "/content"))
        .ok_or(ProviderError::Invalid)?;
    Ok(page(
        url,
        content,
        str_at(item, "/title"),
        "text/markdown",
        "markdown",
    ))
}

/// AnySearch and DeepSeek both answer `{title?, content}`; AnySearch may wrap
/// it in `results[0]`.
fn any_page(url: &str, value: &Value, _provider: &str) -> Result<ProviderPage, ProviderError> {
    // AnySearch answers `{data: {url, title, content}}`; DeepSeek answers
    // `{title, content}` and older AnySearch shapes used `results[0]`.
    let value = value
        .get("data")
        .filter(|data| data.get("content").is_some())
        .unwrap_or(value);
    let node = if value.get("content").is_some() {
        value
    } else {
        value
            .get("results")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .ok_or(ProviderError::Invalid)?
    };
    let content = str_at(node, "/content")
        .or_else(|| str_at(node, "/raw_content"))
        .ok_or(ProviderError::Invalid)?;
    Ok(page(
        url,
        content,
        str_at(node, "/title"),
        "text/markdown",
        "markdown",
    ))
}

fn is_http_url(value: &str) -> bool {
    reqwest::Url::parse(value).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn each_provider_envelope_and_snippet_field_is_read() {
        let cases = [
            (
                ProviderKind::AnySearch,
                json!({"results":[{"title":"A","url":"https://a","snippet":"anysnip","content":"x"}]}),
                "anysnip",
                None,
            ),
            (
                ProviderKind::Brave,
                json!({"web":{"results":[{"title":"A","url":"https://a","description":"desc","extra_snippets":["more","even more"],"page_age":"2026-09-12T00:00:00","age":"1 day ago"}]}}),
                "desc more even more",
                Some("2026-09-12T00:00:00"),
            ),
            (
                ProviderKind::DeepSeek,
                json!({"results":[{"title":"A","url":"https://a","snippet":"","summary":"sum text","date":"2026-09-01"}]}),
                "sum text",
                Some("2026-09-01"),
            ),
            (
                ProviderKind::Exa,
                json!({"results":[{"title":"A","url":"https://a","highlights":["first","second"],"publishedDate":"2026-08-01"}]}),
                "first … second",
                Some("2026-08-01"),
            ),
            (
                ProviderKind::Firecrawl,
                json!({"data":{"web":[{"title":"A","url":"https://a","description":"fcdesc"}]}}),
                "fcdesc",
                None,
            ),
            (
                ProviderKind::Firecrawl,
                json!({"data":[{"title":"A","url":"https://a","description":"flat"}]}),
                "flat",
                None,
            ),
            (
                ProviderKind::Tavily,
                json!({"results":[{"title":"A","url":"https://a","content":"tav","published_date":"2026-07-01"}]}),
                "tav",
                Some("2026-07-01"),
            ),
            (
                ProviderKind::Searxng,
                json!({"results":[{"title":"A","url":"https://a","content":"sx","publishedDate":"2026-06-01"}]}),
                "sx",
                Some("2026-06-01"),
            ),
        ];
        for (kind, value, snippet, published) in cases {
            let hits = parse_hits(kind, &value).unwrap();
            assert_eq!(hits.len(), 1, "{kind:?}");
            assert_eq!(hits[0].url, "https://a", "{kind:?}");
            assert_eq!(hits[0].snippet, snippet, "{kind:?}");
            assert_eq!(hits[0].published_at.as_deref(), published, "{kind:?}");
            assert_eq!(hits[0].provider_rank, 1, "{kind:?}");
        }
    }

    #[test]
    fn anysearch_envelope_is_the_live_data_wrapper() {
        // Live shape (2026-10-30): `{code, message, request_id, data:{results}}`.
        let value = json!({
            "code": 0,
            "message": "ok",
            "data": {"results": [
                {"title": "A", "url": "https://a", "snippet": "a-snip"},
                {"title": "B", "url": "https://b", "content": "b-content"},
            ], "metadata": {}}
        });
        let hits = parse_hits(ProviderKind::AnySearch, &value).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].snippet, "a-snip");
        assert_eq!(hits[1].snippet, "b-content");

        let page = parse_page(
            ProviderKind::AnySearch,
            "https://a",
            json!({"code": 0, "data": {"url": "https://a", "title": "A", "content": "body"}}),
        )
        .unwrap();
        assert_eq!(page.content, "body");
        assert_eq!(page.title.as_deref(), Some("A"));
    }

    #[test]
    fn missing_envelope_is_invalid_not_empty() {
        for kind in [
            ProviderKind::AnySearch,
            ProviderKind::Brave,
            ProviderKind::Firecrawl,
            ProviderKind::Searxng,
        ] {
            assert_eq!(
                parse_hits(kind, &json!({"unexpected": true})),
                Err(ProviderError::Invalid),
                "{kind:?}"
            );
        }
        assert_eq!(
            parse_hits(ProviderKind::Exa, &json!({"results": []})),
            Ok(Vec::new())
        );
    }

    #[test]
    fn hits_without_a_url_are_skipped_and_ranks_stay_dense() {
        let hits = parse_hits(
            ProviderKind::Tavily,
            &json!({"results":[{"title":"no url"},{"url":"https://a"},{"url":"https://b","title":" "}]}),
        )
        .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits.iter().map(|hit| hit.provider_rank).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(hits[1].title, "https://b");
    }

    #[test]
    fn snippets_are_whitespace_collapsed_and_capped() {
        let long = format!("a  b\n{}", "x".repeat(900));
        let hits = parse_hits(
            ProviderKind::Searxng,
            &json!({"results":[{"url":"https://a","content":long}]}),
        )
        .unwrap();
        assert!(hits[0].snippet.starts_with("a b x"));
        assert_eq!(hits[0].snippet.chars().count(), SNIPPET_CHARS);
    }

    #[test]
    fn firecrawl_page_keeps_status_type_source_and_cache_state() {
        let value = json!({"data":{"markdown":"# Doc","metadata":{
            "title":"T","statusCode":203,"contentType":"Text/HTML; charset=utf-8",
            "sourceURL":"https://example.com/final","cachedAt":"2026-10-26T01:00:00Z"}}});
        let page = parse_page(ProviderKind::Firecrawl, "https://example.com/", value).unwrap();
        assert_eq!(page.content, "# Doc");
        assert_eq!(page.title.as_deref(), Some("T"));
        assert_eq!(page.status, 203);
        assert_eq!(page.content_type, "text/html");
        assert_eq!(page.final_url, "https://example.com/final");
        assert_eq!(
            page.freshness.as_deref(),
            Some("cached:2026-10-26T01:00:00Z")
        );
        assert_eq!(page.format, "markdown");

        let live = parse_page(
            ProviderKind::Firecrawl,
            "https://example.com/",
            json!({"data":{"markdown":"x","metadata":{"sourceURL":"javascript:alert(1)"}}}),
        )
        .unwrap();
        assert_eq!(live.final_url, "https://example.com/");
        assert_eq!(live.freshness.as_deref(), Some("live"));
    }

    #[test]
    fn exa_page_is_plain_text_and_reports_cache_source() {
        let cached = parse_page(
            ProviderKind::Exa,
            "https://example.com/",
            json!({"results":[{"url":"https://example.com/","title":"T","text":"body"}],
                   "statuses":[{"id":"https://example.com/","status":"success","source":"cached"}]}),
        )
        .unwrap();
        assert_eq!(cached.content_type, "text/plain");
        assert_eq!(cached.format, "text");
        assert_eq!(cached.freshness.as_deref(), Some("cached"));
        let live = parse_page(
            ProviderKind::Exa,
            "https://example.com/",
            json!({"results":[{"text":"body"}]}),
        )
        .unwrap();
        assert_eq!(live.freshness.as_deref(), Some("live"));
    }

    #[test]
    fn remaining_fetch_envelopes_and_unknown_freshness() {
        let url = "https://example.com/";
        let tavily = parse_page(
            ProviderKind::Tavily,
            url,
            json!({"results":[{"url":url,"raw_content":"tav body"}],"failed_results":[]}),
        )
        .unwrap();
        assert_eq!(tavily.content, "tav body");
        assert_eq!(tavily.freshness.as_deref(), Some("unknown"));
        let any = parse_page(
            ProviderKind::AnySearch,
            url,
            json!({"results":[{"url":url,"title":"T","content":"any body"}]}),
        )
        .unwrap();
        assert_eq!(
            (any.content.as_str(), any.title.as_deref()),
            ("any body", Some("T"))
        );
        let deepseek = parse_page(
            ProviderKind::DeepSeek,
            url,
            json!({"title":"D","content":"ds body"}),
        )
        .unwrap();
        assert_eq!(deepseek.content, "ds body");
    }

    #[test]
    fn empty_or_missing_page_content_is_invalid_and_search_only_kinds_are_unsupported() {
        let url = "https://example.com/";
        assert_eq!(
            parse_page(ProviderKind::DeepSeek, url, json!({"content":"  "})),
            Err(ProviderError::Invalid)
        );
        assert_eq!(
            parse_page(
                ProviderKind::Tavily,
                url,
                json!({"results":[],"failed_results":[{"url":url}]})
            ),
            Err(ProviderError::Invalid)
        );
        for kind in [ProviderKind::Brave, ProviderKind::Searxng] {
            assert_eq!(
                parse_page(kind, url, json!({"content":"x"})),
                Err(ProviderError::Unsupported)
            );
        }
    }
}
