//! `nyan web search|fetch`: non-interactive WebSearch / WebFetch for scripts.
//!
//! @nyan: a thin front end over `services::web` and the WebFetch tool's
//! verbatim page read, so ranking, SSRF policy, snapshots, provider fallback
//! and `range` continuation are the same as inside a session. stdout carries
//! only the result; every failure is one stderr line and a distinct exit code.

use std::io::Write;
use std::time::Duration;

use serde_json::json;

use crate::services::web::search::{SearchError, SearchOutcome};
use crate::services::web::snapshot::RangeSpec;
use crate::tool::AbortController;
use crate::tools::web_fetch_tool::{Origin, PageRead, ReadError};
use crate::utils::settings::nyan::WebSettings;

/// One stdout write stays under this many bytes.
pub(crate) const MAX_OUTPUT_BYTES: usize = 30_000;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_TIMEOUT_SECS: f64 = 600.0;
const DEFAULT_LIMIT: usize = 8;
const MAX_LIMIT: usize = 20;

pub const HELP: &str = "\
Usage: nyan web search <query...> [options]
       nyan web fetch <url> [options]

Non-interactive web search and page fetch using the nyan.web settings
(providers, decision, rerank, apiKeyFile key files, proxy) - no login,
session or TUI. Results go to stdout, errors to stderr.

Commands:
  search <query...>   Search every enabled provider; words are joined by spaces
  fetch <url>         Print the page's original text, one segment at a time.
                      The output ends with the exact command for the next segment.

Options:
      --json             Print JSON instead of Markdown
      --timeout <secs>   Total time limit in seconds (default 15)
      --limit <n>        search: maximum results, 1-20 (default 8)
      --range <spec>     fetch: chars:START-[END], START-[END] or #heading
  -h, --help             Show this help
      --                 Treat every later word as a query word or URL

Exit codes:
  0 success   1 other error   2 usage error (incl. invalid or out-of-range --range)
  3 configuration missing/invalid   4 refused (URL policy, session budget)
  5 network error or every provider failed   124 timeout
";

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Command {
    Search {
        query: String,
        limit: usize,
    },
    Fetch {
        url: String,
        range: Option<RangeSpec>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Invocation {
    pub command: Command,
    pub json: bool,
    pub timeout: Duration,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Parsed {
    Help,
    Run(Invocation),
}

/// Exit codes, documented in [`HELP`].
pub(crate) mod exit {
    pub const OTHER: i32 = 1;
    pub const USAGE: i32 = 2;
    pub const CONFIG: i32 = 3;
    pub const REFUSED: i32 = 4;
    pub const NETWORK: i32 = 5;
    pub const TIMEOUT: i32 = 124;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Failure {
    pub code: i32,
    pub message: String,
}

impl Failure {
    fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// `args` are the words after `web`.
pub(crate) fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut json = false;
    let mut help = false;
    let mut timeout = DEFAULT_TIMEOUT;
    let mut limit = None;
    let mut range = None;
    let mut positional = Vec::new();
    let mut options = true;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        index += 1;
        if !options || !arg.starts_with('-') || arg == "-" {
            positional.push(arg.to_string());
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
            _ => (arg, None),
        };
        let mut value = || -> Result<String, String> {
            if let Some(value) = inline.clone() {
                return Ok(value);
            }
            let value = args
                .get(index)
                .ok_or_else(|| format!("{flag} requires a value"))?;
            index += 1;
            Ok(value.clone())
        };
        match flag {
            "--" => options = false,
            "-h" | "--help" if inline.is_none() => help = true,
            "--json" if inline.is_none() => json = true,
            "--timeout" => timeout = parse_timeout(&value()?)?,
            "--limit" => limit = Some(parse_limit(&value()?)?),
            "--range" => range = Some(parse_range(&value()?)?),
            _ => return Err(format!("unknown option `{arg}`")),
        }
    }
    if help {
        return Ok(Parsed::Help);
    }
    let mut positional = positional.into_iter();
    let command = match positional.next().as_deref() {
        Some("search") => {
            if range.is_some() {
                return Err("--range only applies to `fetch`".to_string());
            }
            let query = positional.collect::<Vec<_>>().join(" ");
            if query.trim().is_empty() {
                return Err("`search` requires a <query>".to_string());
            }
            Command::Search {
                query,
                limit: limit.unwrap_or(DEFAULT_LIMIT),
            }
        }
        Some("fetch") => {
            if limit.is_some() {
                return Err("--limit only applies to `search`".to_string());
            }
            let Some(url) = positional.next() else {
                return Err("`fetch` requires a <url>".to_string());
            };
            if let Some(extra) = positional.next() {
                return Err(format!("unexpected argument `{extra}`"));
            }
            if reqwest::Url::parse(&url).is_err() {
                return Err(format!("invalid URL `{url}`"));
            }
            Command::Fetch { url, range }
        }
        Some(other) => {
            return Err(format!(
                "unknown command `{other}`: expected `search` or `fetch`"
            ));
        }
        None => return Err("missing command: expected `search` or `fetch`".to_string()),
    };
    Ok(Parsed::Run(Invocation {
        command,
        json,
        timeout,
    }))
}

fn parse_timeout(raw: &str) -> Result<Duration, String> {
    match raw.parse::<f64>() {
        Ok(secs) if secs.is_finite() && secs > 0.0 && secs <= MAX_TIMEOUT_SECS => {
            Ok(Duration::from_secs_f64(secs))
        }
        _ => Err(format!(
            "invalid --timeout `{raw}`: expected seconds in (0, {MAX_TIMEOUT_SECS}]"
        )),
    }
}

fn parse_limit(raw: &str) -> Result<usize, String> {
    match raw.parse::<usize>() {
        Ok(limit) if (1..=MAX_LIMIT).contains(&limit) => Ok(limit),
        _ => Err(format!("invalid --limit `{raw}`: expected 1-{MAX_LIMIT}")),
    }
}

/// The WebFetch `range` grammar, plus a bare `START-[END]` for `chars:`.
fn parse_range(raw: &str) -> Result<RangeSpec, String> {
    let spec = if raw.starts_with(|c: char| c.is_ascii_digit()) {
        format!("chars:{raw}")
    } else {
        raw.to_string()
    };
    match crate::services::web::snapshot::parse_range(Some(&spec)) {
        Ok(Some(range)) => Ok(range),
        _ => Err(format!(
            "invalid --range `{raw}`: expected chars:START-[END], START-[END] or #heading"
        )),
    }
}

/// `nyan web …`: `args` are the words after `web`.
pub fn run(args: &[String]) -> i32 {
    let invocation = match parse(args) {
        Ok(Parsed::Help) => {
            print!("{HELP}");
            return 0;
        }
        Ok(Parsed::Run(invocation)) => invocation,
        Err(message) => {
            eprintln!("nyan web: {message}\nRun `nyan web --help` for usage.");
            return exit::USAGE;
        }
    };
    // Same trusted settings env (proxy, CA) and TLS setup the session applies
    // before its first request; project-scoped settings contribute only the
    // safe allowlist, as for `nyan update`.
    crate::utils::tls_provider::install_crypto_provider();
    crate::utils::managed_env::apply_safe_config_environment_variables();
    crate::utils::ca_certs_config::apply_extra_ca_certs_from_config();
    crate::utils::mtls::configure_global_mtls();
    crate::utils::proxy::configure_global_agents();
    let Some(web) = crate::services::web::settings() else {
        eprintln!("nyan web: nyan.web is not configured in settings");
        return exit::CONFIG;
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("nyan web: cannot start the async runtime: {error}");
            return exit::OTHER;
        }
    };
    let result = runtime.block_on(execute(&invocation, &web, None));
    // Dropping the runtime would wait for leftover blocking work (a DNS
    // lookup the timeout abandoned); the total timeout must stay total.
    runtime.shutdown_background();
    match result {
        Ok(output) => {
            let mut stdout = std::io::stdout().lock();
            if stdout
                .write_all(output.as_bytes())
                .and_then(|()| stdout.flush())
                .is_err()
            {
                return exit::OTHER;
            }
            0
        }
        Err(failure) => {
            eprintln!("nyan web: {}", failure.message);
            failure.code
        }
    }
}

/// Runs one invocation under its total timeout and returns the stdout text.
/// `test_config` is the loopback-mock seam; production passes `None`.
pub(crate) async fn execute(
    invocation: &Invocation,
    web: &WebSettings,
    test_config: Option<crate::services::web::fetcher::FetchConfig>,
) -> Result<String, Failure> {
    let abort = AbortController::default();
    let work = async {
        match &invocation.command {
            Command::Search { query, limit } => {
                search(query, *limit, web, &abort, invocation.json).await
            }
            Command::Fetch { url, range } => {
                fetch(
                    url,
                    range.as_ref(),
                    web,
                    &abort,
                    test_config,
                    invocation.json,
                )
                .await
            }
        }
    };
    match tokio::time::timeout(invocation.timeout, work).await {
        Ok(result) => result,
        Err(_) => {
            abort.abort();
            Err(Failure::new(
                exit::TIMEOUT,
                format!(
                    "timeout: no result within {}s (raise it with --timeout)",
                    invocation.timeout.as_secs_f64()
                ),
            ))
        }
    }
}

async fn search(
    query: &str,
    limit: usize,
    web: &WebSettings,
    abort: &AbortController,
    json: bool,
) -> Result<String, Failure> {
    let rerank = crate::services::web::rerank_client(web);
    let outcome =
        crate::services::web::search::search(query, &[], &[], limit, web, rerank.as_ref(), abort)
            .await
            .map_err(search_failure)?;
    Ok(render_search(query, &outcome, json))
}

fn search_failure(error: SearchError) -> Failure {
    let code = match &error {
        SearchError::NoProviders => exit::CONFIG,
        SearchError::Budget(_) => exit::REFUSED,
        SearchError::Unavailable(_) => exit::NETWORK,
        SearchError::Cancelled => exit::OTHER,
    };
    let message = match &error {
        SearchError::Unavailable(_) => format!("all search providers failed: {error}"),
        _ => error.to_string(),
    };
    Failure::new(code, message)
}

/// Drops trailing hits until the rendering fits [`MAX_OUTPUT_BYTES`].
pub(crate) fn render_search(query: &str, outcome: &SearchOutcome, json: bool) -> String {
    let mut shown = outcome.hits.len();
    loop {
        let omitted = outcome.hits.len() - shown;
        let output = if json {
            search_json(query, outcome, shown, omitted)
        } else {
            search_markdown(outcome, shown, omitted)
        };
        if output.len() <= MAX_OUTPUT_BYTES || shown == 0 {
            return output;
        }
        shown -= 1;
    }
}

fn search_markdown(outcome: &SearchOutcome, shown: usize, omitted: usize) -> String {
    let mut output = String::new();
    if outcome.hits.is_empty() {
        output.push_str("No results found.\n");
    }
    for (index, hit) in outcome.hits.iter().take(shown).enumerate() {
        output.push_str(&format!(
            "{}. **{}**\n   {}\n",
            index + 1,
            hit.title,
            hit.url
        ));
        let mut meta = format!("   sources: {}", hit.providers.join(", "));
        if let Some(date) = &hit.published_at {
            meta.push_str(&format!(" · published {date}"));
        }
        if hit.likely_answers {
            meta.push_str(" · likely answers");
        }
        output.push_str(&meta);
        output.push('\n');
        if !hit.snippet.is_empty() {
            output.push_str(&format!("   {}\n", hit.snippet));
        }
        output.push('\n');
    }
    if omitted > 0 {
        output.push_str(&format!(
            "[{omitted} more results omitted to stay under {MAX_OUTPUT_BYTES} bytes]\n"
        ));
    }
    output.push_str(&format!("[searched: {}", outcome.providers_used.join(", ")));
    if !outcome.failures.is_empty() {
        let failures = outcome
            .failures
            .iter()
            .map(|failure| {
                let cooling = if failure.cooling_down {
                    " (cooling down)"
                } else {
                    ""
                };
                format!("{} {}{cooling}", failure.provider, failure.reason)
            })
            .collect::<Vec<_>>();
        output.push_str(&format!(" — failed: {}", failures.join(", ")));
    }
    output.push_str("]\n");
    output
}

fn search_json(query: &str, outcome: &SearchOutcome, shown: usize, omitted: usize) -> String {
    let results = outcome
        .hits
        .iter()
        .take(shown)
        .map(|hit| {
            json!({
                "title": hit.title,
                "url": hit.url,
                "snippet": hit.snippet,
                "published_at": hit.published_at,
                "providers": hit.providers,
                "likely_answers": hit.likely_answers,
            })
        })
        .collect::<Vec<_>>();
    let failures = outcome
        .failures
        .iter()
        .map(|failure| {
            json!({
                "provider": failure.provider,
                "reason": failure.reason,
                "cooling_down": failure.cooling_down,
            })
        })
        .collect::<Vec<_>>();
    let value = json!({
        "query": query,
        "results": results,
        "omitted": omitted,
        "providers_used": outcome.providers_used,
        "failures": failures,
        "reranked": outcome.reranked,
        "query_kind": outcome.query_kind,
    });
    format!("{value}\n")
}

async fn fetch(
    url: &str,
    range: Option<&RangeSpec>,
    web: &WebSettings,
    abort: &AbortController,
    test_config: Option<crate::services::web::fetcher::FetchConfig>,
    json: bool,
) -> Result<String, Failure> {
    let page = crate::tools::web_fetch_tool::read_page(url, range, web, abort, test_config)
        .await
        .map_err(read_failure)?;
    Ok(render_fetch(url, &page, json))
}

fn read_failure(error: ReadError) -> Failure {
    let code = match &error {
        ReadError::Target(_) | ReadError::Budget(_) => exit::REFUSED,
        ReadError::Range(_) => exit::USAGE,
        ReadError::Fetch(failure) if !failure.attempts.is_empty() => exit::NETWORK,
        ReadError::Fetch(failure) => {
            let reason = failure.nyan.as_str();
            if reason.starts_with("proxy_unconfigured") {
                exit::CONFIG
            } else if [
                "denied_",
                "content_type_denied",
                "too_large",
                "too_many_redirects",
            ]
            .iter()
            .any(|prefix| reason.starts_with(prefix))
            {
                exit::REFUSED
            } else if reason.starts_with("snapshot_error") || reason == "cancelled" {
                exit::OTHER
            } else {
                exit::NETWORK
            }
        }
    };
    Failure::new(code, error.to_string())
}

/// Renders one segment. The segment is `maxReturnChars` long at most (the
/// WebFetch tool's cap) and shrinks further until the whole output fits
/// [`MAX_OUTPUT_BYTES`]; the continuation always starts where it ended.
pub(crate) fn render_fetch(url: &str, page: &PageRead, json: bool) -> String {
    let chars = page.selected.text.chars().collect::<Vec<_>>();
    let mut len = chars.len();
    loop {
        let body = chars[..len].iter().collect::<String>();
        let end = page.selected.start + len;
        let output = if json {
            fetch_json(url, page, &body, end)
        } else {
            fetch_markdown(url, page, &body, end)
        };
        if output.len() <= MAX_OUTPUT_BYTES || len == 0 {
            return output;
        }
        let scaled = (len as u128 * MAX_OUTPUT_BYTES as u128 / output.len() as u128) as usize;
        len = scaled.min(len - 1);
    }
}

fn origin_name(origin: Origin) -> &'static str {
    match origin {
        Origin::Fresh => "fresh",
        Origin::Cached => "cached",
        Origin::Continuation => "continuation",
    }
}

/// `nyan web fetch <url> --range <end>-`, shell-quoted for `bash -c`.
pub(crate) fn next_command(url: &str, end: usize, json: bool) -> String {
    format!(
        "nyan web fetch {} --range {end}-{}",
        crate::utils::swarm::spawn_utils::shell_quote(url),
        if json { " --json" } else { "" }
    )
}

fn fetch_markdown(url: &str, page: &PageRead, body: &str, end: usize) -> String {
    let metadata = &page.snapshot.metadata;
    let total = page.selected.total;
    let mut output = format!(
        "{}  (fetched {} by {}; status {}; {}; {})\n",
        metadata.final_url,
        metadata.fetched_at.to_rfc3339(),
        metadata.fetched_by,
        metadata.status,
        metadata.content_type,
        origin_name(page.origin),
    );
    if let Some(reason) = &page.direct_reason {
        output.push_str(&format!("[direct fetch failed: {reason}]\n"));
    }
    if page.challenge_fallback_failed {
        output.push_str("[provider fallback failed; serving the direct fetch]\n");
    }
    if let Some(section) = &page.selected.section {
        output.push_str(&format!("[section: {section}]\n"));
    }
    output.push_str(&format!(
        "[chars {}–{end} of {total}]\n\n<web_content source=\"{}\">\n{body}\n</web_content>\n\n",
        page.selected.start, metadata.final_url,
    ));
    if end < total {
        output.push_str(&format!(
            "[next segment: {}]\n",
            next_command(url, end, false)
        ));
    } else {
        output.push_str("[end of page]\n");
    }
    output
}

fn fetch_json(url: &str, page: &PageRead, body: &str, end: usize) -> String {
    let metadata = &page.snapshot.metadata;
    let total = page.selected.total;
    let next = (end < total).then(|| {
        json!({
            "range": format!("{end}-"),
            "command": next_command(url, end, true),
        })
    });
    let value = json!({
        "url": url,
        "final_url": metadata.final_url,
        "status": metadata.status,
        "content_type": metadata.content_type,
        "fetched_at": metadata.fetched_at.to_rfc3339(),
        "fetched_by": metadata.fetched_by,
        "origin": origin_name(page.origin),
        "direct_fetch_failed": page.direct_reason,
        "provider_fallback_failed": page.challenge_fallback_failed,
        "section": page.selected.section,
        "total_chars": total,
        "start": page.selected.start,
        "end": end,
        "content": body,
        "next": next,
    });
    format!("{value}\n")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{Value, json};

    use super::*;
    use crate::services::web::search::{SearchFailure, SearchHit};
    use crate::services::web::test_support::{
        Route, WebSettingsGuard, key_file, only_mocked_providers, serve,
    };
    use crate::services::web::{budget, fetcher::FetchConfig, provider_health};
    use crate::utils::test_env::EnvVarGuard;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_string()).collect()
    }

    fn run_of(words: &[&str]) -> Invocation {
        match parse(&args(words)) {
            Ok(Parsed::Run(invocation)) => invocation,
            other => panic!("{words:?}: {other:?}"),
        }
    }

    // ── parsing ────────────────────────────────────────────────

    #[test]
    fn search_joins_query_words_and_uses_the_defaults() {
        let invocation = run_of(&["search", "rust", "async", "book"]);
        assert_eq!(
            invocation,
            Invocation {
                command: Command::Search {
                    query: "rust async book".to_string(),
                    limit: 8,
                },
                json: false,
                timeout: Duration::from_secs(15),
            }
        );
    }

    #[test]
    fn options_are_accepted_anywhere_in_both_forms() {
        let invocation = run_of(&["--json", "search", "q", "--limit", "3", "--timeout=2.5"]);
        assert!(invocation.json);
        assert_eq!(invocation.timeout, Duration::from_millis(2500));
        assert_eq!(
            invocation.command,
            Command::Search {
                query: "q".to_string(),
                limit: 3
            }
        );

        let invocation = run_of(&["fetch", "https://example.com/a", "--range", "100-200"]);
        assert_eq!(
            invocation.command,
            Command::Fetch {
                url: "https://example.com/a".to_string(),
                range: Some(RangeSpec::Chars {
                    start: 100,
                    end: Some(200)
                }),
            }
        );
        for (spec, expected) in [
            (
                "chars:5-",
                RangeSpec::Chars {
                    start: 5,
                    end: None,
                },
            ),
            (
                "5-",
                RangeSpec::Chars {
                    start: 5,
                    end: None,
                },
            ),
            ("#usage", RangeSpec::Section("usage".to_string())),
        ] {
            let invocation = run_of(&["fetch", "https://example.com/", &format!("--range={spec}")]);
            assert!(
                matches!(&invocation.command, Command::Fetch { range: Some(range), .. } if *range == expected),
                "{spec}"
            );
        }
    }

    #[test]
    fn the_option_terminator_keeps_dashed_words_in_the_query() {
        let invocation = run_of(&["search", "--", "--json", "-x"]);
        assert!(!invocation.json);
        assert_eq!(
            invocation.command,
            Command::Search {
                query: "--json -x".to_string(),
                limit: 8
            }
        );
    }

    #[test]
    fn help_wins_over_everything_else() {
        for words in [
            vec!["--help"],
            vec!["-h"],
            vec!["search", "--help"],
            vec!["fetch", "-h"],
            vec![],
        ] {
            let parsed = parse(&args(&words));
            if words.is_empty() {
                assert!(parsed.is_err(), "no command is a usage error");
            } else {
                assert_eq!(parsed, Ok(Parsed::Help), "{words:?}");
            }
        }
        assert!(HELP.contains("nyan web fetch <url>"));
        assert!(HELP.contains("--timeout <secs>"));
        assert!(HELP.contains("124 timeout"));
    }

    #[test]
    fn usage_errors_are_named() {
        for (words, expected) in [
            (vec!["search"], "requires a <query>"),
            (vec!["search", "  "], "requires a <query>"),
            (vec!["fetch"], "requires a <url>"),
            (vec!["fetch", "not a url"], "invalid URL"),
            (
                vec!["fetch", "https://a.example/", "extra"],
                "unexpected argument",
            ),
            (vec!["browse", "x"], "unknown command `browse`"),
            (vec!["search", "q", "--bogus"], "unknown option `--bogus`"),
            (
                vec!["search", "q", "--json=yes"],
                "unknown option `--json=yes`",
            ),
            (
                vec!["search", "q", "--timeout"],
                "--timeout requires a value",
            ),
            (vec!["search", "q", "--timeout", "0"], "invalid --timeout"),
            (vec!["search", "q", "--timeout", "abc"], "invalid --timeout"),
            (vec!["search", "q", "--limit", "21"], "invalid --limit"),
            (vec!["search", "q", "--limit", "0"], "invalid --limit"),
            (vec!["search", "q", "--range", "0-"], "--range only applies"),
            (
                vec!["fetch", "https://a.example/", "--limit", "2"],
                "--limit only applies",
            ),
            (
                vec!["fetch", "https://a.example/", "--range", "9-3"],
                "invalid --range",
            ),
            (
                vec!["fetch", "https://a.example/", "--range", "x"],
                "invalid --range",
            ),
        ] {
            let error = parse(&args(&words)).unwrap_err();
            assert!(error.contains(expected), "{words:?}: {error}");
        }
    }

    #[test]
    fn a_usage_error_exits_two_before_any_setup() {
        assert_eq!(run(&args(&["search"])), exit::USAGE);
        assert_eq!(run(&args(&["search", "q", "--nope"])), exit::USAGE);
        assert_eq!(run(&args(&["--help"])), 0);
    }

    #[test]
    fn the_top_level_parser_routes_web_and_its_help() {
        let argv = |words: &[&str]| {
            std::iter::once("nyan".to_string())
                .chain(words.iter().map(|word| (*word).to_string()))
                .collect::<Vec<_>>()
        };
        let config = crate::cli::parse_cli_config(&argv(&["web", "search", "q", "--json"]));
        assert_eq!(config.subcommand.as_deref(), Some("web"));
        assert_eq!(config.subcommand_args, args(&["search", "q", "--json"]));
        assert!(config.unimplemented_reasons().is_empty());
        // `--help` after `web` belongs to `web`, not the top-level help.
        let config = crate::cli::parse_cli_config(&argv(&["web", "fetch", "--help"]));
        assert!(!config.show_help);
        assert_eq!(crate::cli::dispatch(&config), Some(0));
        let config = crate::cli::parse_cli_config(&argv(&["web", "fetch"]));
        assert_eq!(crate::cli::dispatch(&config), Some(exit::USAGE));
    }

    // ── search rendering ───────────────────────────────────────

    fn hit(index: usize, snippet: &str) -> SearchHit {
        SearchHit {
            title: format!("Title {index}"),
            url: format!("https://example.com/{index}"),
            snippet: snippet.to_string(),
            published_at: (index == 1).then(|| "2026-08-01".to_string()),
            providers: vec!["exa".to_string(), "tavily".to_string()],
            provider_count: 2,
            rrf_rank: index,
            likely_answers: index == 1,
        }
    }

    fn outcome(hits: Vec<SearchHit>) -> SearchOutcome {
        SearchOutcome {
            hits,
            failures: vec![SearchFailure {
                provider: "brave".to_string(),
                reason: "rate_limited".to_string(),
                cooling_down: true,
            }],
            providers_used: vec!["exa".to_string(), "tavily".to_string()],
            all_failed: false,
            reranked: false,
            query_kind: None,
        }
    }

    #[test]
    fn search_markdown_lists_title_url_sources_and_snippet() {
        let rendered = render_search("q", &outcome(vec![hit(1, "first"), hit(2, "")]), false);
        assert_eq!(
            rendered,
            "1. **Title 1**\n   https://example.com/1\n   sources: exa, tavily · published 2026-08-01 · likely answers\n   first\n\n\
             2. **Title 2**\n   https://example.com/2\n   sources: exa, tavily\n\n\
             [searched: exa, tavily — failed: brave rate_limited (cooling down)]\n"
        );
        let empty = render_search("q", &outcome(Vec::new()), false);
        assert!(empty.starts_with("No results found.\n"), "{empty}");
    }

    #[test]
    fn search_json_has_results_and_provider_status() {
        let rendered = render_search("q", &outcome(vec![hit(1, "first")]), true);
        assert!(rendered.ends_with('\n'));
        let value: Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(value["query"], "q");
        assert_eq!(value["omitted"], 0);
        assert_eq!(
            value["results"][0],
            json!({
                "title": "Title 1",
                "url": "https://example.com/1",
                "snippet": "first",
                "published_at": "2026-08-01",
                "providers": ["exa", "tavily"],
                "likely_answers": true,
            })
        );
        assert_eq!(value["providers_used"], json!(["exa", "tavily"]));
        assert_eq!(
            value["failures"],
            json!([{"provider": "brave", "reason": "rate_limited", "cooling_down": true}])
        );
    }

    #[test]
    fn search_output_drops_trailing_hits_to_stay_under_the_cap() {
        let big = "x".repeat(4_000);
        let hits = (1..=20).map(|index| hit(index, &big)).collect::<Vec<_>>();
        for json in [false, true] {
            let rendered = render_search("q", &outcome(hits.clone()), json);
            assert!(rendered.len() <= MAX_OUTPUT_BYTES, "{}", rendered.len());
            if json {
                let value: Value = serde_json::from_str(&rendered).unwrap();
                let shown = value["results"].as_array().unwrap().len();
                assert!(shown > 0 && shown < 20);
                assert_eq!(value["omitted"], 20 - shown);
            } else {
                assert!(rendered.contains("1. **Title 1**"));
                assert!(!rendered.contains("Title 20"));
                assert!(rendered.contains("more results omitted"), "{rendered}");
            }
        }
    }

    // ── against local mocks ────────────────────────────────────

    /// Holds the env lock (via the settings guard), then the provider-health
    /// lock, in the order `utils::test_env` prescribes.
    struct Env {
        dir: std::path::PathBuf,
        _config_home: EnvVarGuard,
        _health: std::sync::MutexGuard<'static, ()>,
        _settings: WebSettingsGuard,
    }

    impl Drop for Env {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn env(name: &str, web: &WebSettings) -> Env {
        let settings = WebSettingsGuard::install(web.clone());
        let health = provider_health::test_lock();
        budget::reset();
        let dir = std::env::temp_dir().join(format!(
            "nyan-web-cli-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Env {
            _config_home: EnvVarGuard::set("NYAN_CONFIG_DIR", &dir),
            dir,
            _health: health,
            _settings: settings,
        }
    }

    fn settings(value: Value) -> WebSettings {
        serde_json::from_value(value).unwrap()
    }

    async fn exec(words: &[&str], web: &WebSettings) -> Result<String, Failure> {
        execute(&run_of(words), web, Some(FetchConfig::for_local_mock())).await
    }

    fn page_text() -> String {
        // 2 500 chars of distinct lines, so a segment's position is checkable.
        (0..50)
            .map(|line| format!("line {line:04} {}\n", "y".repeat(39)))
            .collect()
    }

    #[tokio::test]
    async fn fetch_reads_segment_by_segment_to_the_end() {
        let web = settings(json!({"maxReturnChars": 1000, "fetchFallback": []}));
        let _env = env("segments", &web);
        let text = page_text();
        let total = text.chars().count();
        let server = serve(vec![("/doc", Route::text(&text))]).await;
        let url = server.url("/doc");

        let first = exec(&["fetch", &url], &web).await.unwrap();
        assert!(first.starts_with(&format!("{url}  (fetched ")), "{first}");
        assert!(
            first.contains(&format!("[chars 0–1000 of {total}]")),
            "{first}"
        );
        assert!(first.contains(&format!(
            "<web_content source=\"{url}\">\n{}",
            &text[..1000]
        )));
        assert!(first.contains(&format!(
            "[next segment: nyan web fetch {url} --range 1000-]"
        )));

        let middle = exec(&["fetch", &url, "--range", "1000-"], &web)
            .await
            .unwrap();
        assert!(
            middle.contains(&format!("[chars 1000–2000 of {total}]")),
            "{middle}"
        );
        assert!(middle.contains(&text[1000..2000]));
        assert!(middle.contains("continuation"));
        assert!(middle.contains(&format!("--range 2000-]")));

        let last = exec(&["fetch", &url, "--range", "2000-"], &web)
            .await
            .unwrap();
        assert!(
            last.contains(&format!("[chars 2000–{total} of {total}]")),
            "{last}"
        );
        assert!(last.contains(&text[2000..]));
        assert!(last.ends_with("[end of page]\n"), "{last}");
        assert_eq!(server.hits("/doc"), 1, "continuations read the snapshot");

        let json = exec(&["fetch", &url, "--range", "1000-", "--json"], &web)
            .await
            .unwrap();
        let value: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["url"], url.as_str());
        assert_eq!(value["total_chars"], total);
        assert_eq!(value["start"], 1000);
        assert_eq!(value["end"], 2000);
        assert_eq!(value["content"], &text[1000..2000]);
        assert_eq!(value["origin"], "continuation");
        assert_eq!(value["status"], 200);
        assert_eq!(
            value["next"],
            json!({"range": "2000-", "command": format!("nyan web fetch {url} --range 2000- --json")})
        );
        let tail = exec(&["fetch", &url, "--range", "2000-", "--json"], &web)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&tail).unwrap()["next"],
            Value::Null
        );

        let over = exec(
            &["fetch", &url, "--range", &format!("{}-", total + 1)],
            &web,
        )
        .await
        .unwrap_err();
        assert_eq!(over.code, exit::USAGE);
        assert!(
            over.message.contains("range_out_of_bounds"),
            "{}",
            over.message
        );
    }

    #[tokio::test]
    async fn a_wide_segment_shrinks_under_the_cap_and_continues_where_it_stopped() {
        let web = settings(json!({"maxReturnChars": 20000, "fetchFallback": []}));
        let _env = env("cap", &web);
        // Three bytes per char: 20 000 chars would be 60 KB.
        let text = "中".repeat(25_000);
        let server = serve(vec![("/wide", Route::text(&text))]).await;
        let url = server.url("/wide");
        for json in [false, true] {
            let mut words = vec!["fetch", url.as_str()];
            if json {
                words.push("--json");
            }
            let output = exec(&words, &web).await.unwrap();
            assert!(output.len() <= MAX_OUTPUT_BYTES, "{}", output.len());
            let end = if json {
                serde_json::from_str::<Value>(&output).unwrap()["end"]
                    .as_u64()
                    .unwrap() as usize
            } else {
                let marker = "--range ";
                let at = output.rfind(marker).unwrap() + marker.len();
                output[at..].split('-').next().unwrap().parse().unwrap()
            };
            assert!(end > 5_000 && end < 20_000, "{end}");
            if !json {
                assert!(output.contains(&format!("[chars 0–{end} of 25000]")));
            }
            let next = exec(&["fetch", &url, "--range", &format!("{end}-")], &web)
                .await
                .unwrap();
            assert!(next.contains(&format!("[chars {end}–")), "{next}");
            assert!(next.len() <= MAX_OUTPUT_BYTES);
        }
    }

    #[tokio::test]
    async fn fetch_failures_have_distinct_exit_codes_and_reasons() {
        let web = settings(json!({"fetchFallback": []}));
        let _env = env("errors", &web);
        let server = serve(vec![
            ("/missing", Route::new(404, "text/plain", "gone")),
            (
                "/slow",
                Route::text(&page_text()).delayed(Duration::from_secs(5)),
            ),
        ])
        .await;

        let missing = exec(&["fetch", &server.url("/missing")], &web)
            .await
            .unwrap_err();
        assert_eq!(missing.code, exit::NETWORK);
        assert_eq!(missing.message, "fetch_failed: nyan http_404");

        let refused = execute(
            &run_of(&["fetch", "https://127.0.0.1/x"]),
            &web,
            Some(FetchConfig::from_settings(&web)),
        )
        .await
        .unwrap_err();
        assert_eq!(refused.code, exit::REFUSED);
        assert!(refused.message.starts_with("denied_private_network"));

        let started = std::time::Instant::now();
        let slow = exec(&["fetch", &server.url("/slow"), "--timeout", "0.3"], &web)
            .await
            .unwrap_err();
        assert_eq!(slow.code, exit::TIMEOUT);
        assert!(
            slow.message.starts_with("timeout: no result within 0.3s"),
            "{}",
            slow.message
        );
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    fn provider(origin: &str) -> Value {
        json!({"enabled": true, "baseUrl": origin, "apiKeyFile": key_file()})
    }

    #[tokio::test]
    async fn search_runs_the_configured_providers() {
        let server = serve(vec![(
            "/search",
            Route::new(
                200,
                "application/json",
                json!({"results": [{"title": "Mock", "url": "https://example.com/m", "highlights": ["hello"]}]})
                    .to_string(),
            ),
        )])
        .await;
        let web = settings(
            json!({"providers": only_mocked_providers(json!({"exa": provider(&server.origin)}))}),
        );
        let _env = env("search", &web);
        let markdown = exec(&["search", "mock", "query"], &web).await.unwrap();
        assert!(
            markdown
                .starts_with("1. **Mock**\n   https://example.com/m\n   sources: exa\n   hello\n"),
            "{markdown}"
        );
        assert!(markdown.ends_with("[searched: exa]\n"), "{markdown}");
        let json = exec(&["search", "mock", "--json", "--limit", "1"], &web)
            .await
            .unwrap();
        let value: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["results"][0]["url"], "https://example.com/m");
        assert!(
            server
                .seen()
                .iter()
                .all(|seen| !seen.body.contains("test-key-not-real")),
            "the key travels in a header only"
        );
    }

    #[tokio::test]
    async fn search_failures_name_every_provider_and_never_the_key() {
        let down = serve(vec![(
            "/search",
            Route::new(500, "application/json", "boom test-key-not-real"),
        )])
        .await;
        let web = settings(
            json!({"providers": only_mocked_providers(json!({"exa": provider(&down.origin)}))}),
        );
        let _env = env("search-down", &web);
        let failure = exec(&["search", "q"], &web).await.unwrap_err();
        assert_eq!(failure.code, exit::NETWORK);
        assert!(
            failure
                .message
                .starts_with("all search providers failed: unavailable: exa "),
            "{}",
            failure.message
        );
        assert!(!failure.message.contains("test-key-not-real"));

        let slow = serve(vec![(
            "/search",
            Route::new(200, "application/json", "{\"results\":[]}").delayed(Duration::from_secs(5)),
        )])
        .await;
        let web = settings(
            json!({"providers": only_mocked_providers(json!({"exa": provider(&slow.origin)}))}),
        );
        let timeout = execute(&run_of(&["search", "q", "--timeout", "0.3"]), &web, None)
            .await
            .unwrap_err();
        assert_eq!(timeout.code, exit::TIMEOUT);

        let none = settings(json!({"providers": only_mocked_providers(json!({}))}));
        let failure = exec(&["search", "q"], &none).await.unwrap_err();
        assert_eq!(failure.code, exit::CONFIG);
        assert!(
            failure.message.contains("no web search providers"),
            "{}",
            failure.message
        );
    }

    #[test]
    fn the_next_command_is_shell_quoted() {
        assert_eq!(
            next_command("https://e.com/a?b=1&c='x'", 40, false),
            "nyan web fetch 'https://e.com/a?b=1&c='\\''x'\\''' --range 40-"
        );
        assert_eq!(
            next_command("https://e.com/a", 7, true),
            "nyan web fetch https://e.com/a --range 7- --json"
        );
    }
}
