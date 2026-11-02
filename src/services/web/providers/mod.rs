//! Search and fetch provider adapters.
//!
//! @nyan: provider calls receive only the model's query or the explicitly
//! requested URL. Keys are read from configured files, never from settings JSON
//! or request input. Provider response bodies are parsed into neutral types and
//! raw error bodies never cross this boundary.
//!
//! Layout: `client` owns the wire (request bodies, auth, deadlines, health),
//! `parse` owns response shapes, `errors` owns status → [`ProviderError`].

use std::time::Duration;

use futures::future::BoxFuture;

use crate::utils::settings::nyan::WebSettings;

use client::ProviderClient;

mod client;
mod errors;
mod parse;
#[cfg(test)]
mod tests;

/// Query classification from `web.query_kind` (decision-provider 4.2). It only
/// adds per-provider parameters; every enabled provider is still queried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryKind {
    Chinese,
    TechDocs,
    News,
    General,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchRequest {
    pub query: String,
    pub max_results: usize,
    pub allowed_domains: Vec<String>,
    pub blocked_domains: Vec<String>,
    /// `None` until the Decision Provider answers with enough confidence.
    pub query_kind: Option<QueryKind>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub published_at: Option<String>,
    pub provider_rank: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderPage {
    pub url: String,
    pub final_url: String,
    pub title: Option<String>,
    pub content: String,
    pub status: u16,
    pub content_type: String,
    pub format: String,
    pub freshness: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderError {
    Timeout,
    QuotaExhausted,
    RateLimited,
    Auth,
    Http(u16),
    Invalid,
    Unsupported,
    Unavailable,
    Network,
    BudgetExhausted,
}

impl ProviderError {
    pub fn code(&self) -> String {
        match self {
            Self::Timeout => "timeout".to_string(),
            Self::QuotaExhausted => "quota_exhausted".to_string(),
            Self::BudgetExhausted => "budget_exhausted".to_string(),
            Self::RateLimited => "rate_limited".to_string(),
            Self::Auth => "auth_failed".to_string(),
            Self::Http(status) => format!("http_{status}"),
            Self::Invalid => "invalid_response".to_string(),
            Self::Unsupported => "unsupported".to_string(),
            Self::Unavailable => "unavailable".to_string(),
            Self::Network => "network_error".to_string(),
        }
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.code())
    }
}

impl std::error::Error for ProviderError {}

pub trait SearchProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn max_results(&self) -> usize;
    fn search<'a>(
        &'a self,
        request: SearchRequest,
        deadline: Duration,
    ) -> BoxFuture<'a, Result<Vec<Hit>, ProviderError>>;
}

pub trait FetchProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn fetch<'a>(
        &'a self,
        url: String,
        deadline: Duration,
    ) -> BoxFuture<'a, Result<ProviderPage, ProviderError>>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProviderKind {
    AnySearch,
    Brave,
    DeepSeek,
    Exa,
    Firecrawl,
    Tavily,
    Searxng,
}

impl ProviderKind {
    fn name(self) -> &'static str {
        match self {
            Self::AnySearch => "anysearch",
            Self::Brave => "brave",
            Self::DeepSeek => "deepseek",
            Self::Exa => "exa",
            Self::Firecrawl => "firecrawl",
            Self::Tavily => "tavily",
            Self::Searxng => "searxng",
        }
    }

    fn default_base_url(self) -> &'static str {
        match self {
            Self::AnySearch => "https://api.anysearch.com/v1",
            Self::Brave => "https://api.search.brave.com/res/v1/web",
            Self::DeepSeek => "https://api.deepseek.com/v1/tools",
            Self::Exa => "https://api.exa.ai",
            Self::Firecrawl => "https://api.firecrawl.dev/v2",
            Self::Tavily => "https://api.tavily.com",
            Self::Searxng => "http://10.144.0.3:8454",
        }
    }

    /// Per-request result ceiling (external-search 2.3).
    fn max_results(self) -> usize {
        match self {
            Self::AnySearch => 10,
            Self::Brave | Self::Tavily | Self::Searxng => 20,
            Self::DeepSeek => 50,
            Self::Exa | Self::Firecrawl => 100,
        }
    }

    fn requires_key(self) -> bool {
        self != Self::Searxng
    }
}

#[derive(Clone)]
struct GenericProvider {
    kind: ProviderKind,
    client: ProviderClient,
}

impl GenericProvider {
    fn name(&self) -> &'static str {
        self.kind.name()
    }

    fn search<'a>(
        &'a self,
        request: SearchRequest,
        deadline: Duration,
    ) -> BoxFuture<'a, Result<Vec<Hit>, ProviderError>> {
        Box::pin(async move {
            let value = self.client.search(&request, deadline).await?;
            let mut hits = parse::parse_hits(self.kind, &value)?;
            hits.truncate(request.max_results.min(self.kind.max_results()));
            Ok(hits)
        })
    }

    fn fetch<'a>(
        &'a self,
        url: String,
        deadline: Duration,
    ) -> BoxFuture<'a, Result<ProviderPage, ProviderError>> {
        Box::pin(async move {
            let value = self.client.fetch(&url, deadline).await?;
            parse::parse_page(self.kind, &url, value)
        })
    }
}

macro_rules! provider_type {
    ($name:ident, $kind:ident) => {
        #[derive(Clone)]
        pub struct $name {
            inner: GenericProvider,
        }

        impl $name {
            pub fn from_settings(web: &WebSettings) -> Result<Self, ProviderError> {
                let resolved = web.provider(ProviderKind::$kind.name());
                Ok(Self {
                    inner: GenericProvider {
                        kind: ProviderKind::$kind,
                        client: ProviderClient::from_resolved(ProviderKind::$kind, &resolved)?,
                    },
                })
            }

            pub fn with_base_url(
                base_url: impl Into<String>,
                api_key: Option<String>,
            ) -> Result<Self, ProviderError> {
                Ok(Self {
                    inner: GenericProvider {
                        kind: ProviderKind::$kind,
                        client: ProviderClient::with_base_url(
                            ProviderKind::$kind,
                            base_url.into(),
                            api_key,
                        )?,
                    },
                })
            }
        }

        impl SearchProvider for $name {
            fn name(&self) -> &'static str {
                self.inner.name()
            }
            fn max_results(&self) -> usize {
                self.inner.kind.max_results()
            }
            fn search<'a>(
                &'a self,
                request: SearchRequest,
                deadline: Duration,
            ) -> BoxFuture<'a, Result<Vec<Hit>, ProviderError>> {
                self.inner.search(request, deadline)
            }
        }
    };
}

provider_type!(AnySearchProvider, AnySearch);
provider_type!(BraveProvider, Brave);
provider_type!(DeepSeekProvider, DeepSeek);
provider_type!(ExaProvider, Exa);
provider_type!(FirecrawlProvider, Firecrawl);
provider_type!(TavilyProvider, Tavily);
provider_type!(SearxngProvider, Searxng);

macro_rules! fetch_impl {
    ($name:ident) => {
        impl FetchProvider for $name {
            fn name(&self) -> &'static str {
                self.inner.name()
            }
            fn fetch<'a>(
                &'a self,
                url: String,
                deadline: Duration,
            ) -> BoxFuture<'a, Result<ProviderPage, ProviderError>> {
                self.inner.fetch(url, deadline)
            }
        }
    };
}

fetch_impl!(AnySearchProvider);
fetch_impl!(DeepSeekProvider);
fetch_impl!(ExaProvider);
fetch_impl!(FirecrawlProvider);
fetch_impl!(TavilyProvider);

pub enum ConfiguredSearchProvider {
    AnySearch(AnySearchProvider),
    Brave(BraveProvider),
    DeepSeek(DeepSeekProvider),
    Exa(ExaProvider),
    Firecrawl(FirecrawlProvider),
    Tavily(TavilyProvider),
    Searxng(SearxngProvider),
}

impl ConfiguredSearchProvider {
    pub fn as_search(&self) -> &dyn SearchProvider {
        match self {
            Self::AnySearch(value) => value,
            Self::Brave(value) => value,
            Self::DeepSeek(value) => value,
            Self::Exa(value) => value,
            Self::Firecrawl(value) => value,
            Self::Tavily(value) => value,
            Self::Searxng(value) => value,
        }
    }

    pub fn as_fetch(&self) -> Result<&dyn FetchProvider, ProviderError> {
        match self {
            Self::AnySearch(value) => Ok(value),
            Self::Brave(_) | Self::Searxng(_) => Err(ProviderError::Unsupported),
            Self::DeepSeek(value) => Ok(value),
            Self::Exa(value) => Ok(value),
            Self::Firecrawl(value) => Ok(value),
            Self::Tavily(value) => Ok(value),
        }
    }
}

/// An enabled provider that could not be constructed (for example an
/// unreadable key file). It is reported as a named failure, never dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderBuildFailure {
    pub provider: String,
    pub error: ProviderError,
}

const PROVIDER_ORDER: [&str; 7] = [
    "anysearch",
    "brave",
    "deepseek",
    "exa",
    "firecrawl",
    "tavily",
    "searxng",
];

pub fn configured_search_providers(web: &WebSettings) -> Vec<ConfiguredSearchProvider> {
    configured_providers(web).0
}

/// Every builtin provider, plus the ones that failed to build.
///
/// Entries in `providers` override builtins; they do not replace the set. A
/// key the user left out stays at its builtin default, which is enabled. The
/// only way to drop a provider is `enabled: false`. An absent `providers` map
/// still means web search is not configured: nothing is built, and WebSearch
/// stays hidden.
pub fn configured_providers(
    web: &WebSettings,
) -> (Vec<ConfiguredSearchProvider>, Vec<ProviderBuildFailure>) {
    let Some(configured) = web.providers.as_ref() else {
        return (Vec::new(), Vec::new());
    };
    let mut providers = Vec::new();
    let mut failures = Vec::new();
    for name in PROVIDER_ORDER {
        if configured.get(name).and_then(|config| config.enabled) == Some(false) {
            continue;
        }
        let built = match name {
            "anysearch" => {
                AnySearchProvider::from_settings(web).map(ConfiguredSearchProvider::AnySearch)
            }
            "brave" => BraveProvider::from_settings(web).map(ConfiguredSearchProvider::Brave),
            "deepseek" => {
                DeepSeekProvider::from_settings(web).map(ConfiguredSearchProvider::DeepSeek)
            }
            "exa" => ExaProvider::from_settings(web).map(ConfiguredSearchProvider::Exa),
            "firecrawl" => {
                FirecrawlProvider::from_settings(web).map(ConfiguredSearchProvider::Firecrawl)
            }
            "tavily" => TavilyProvider::from_settings(web).map(ConfiguredSearchProvider::Tavily),
            "searxng" => SearxngProvider::from_settings(web).map(ConfiguredSearchProvider::Searxng),
            _ => continue,
        };
        match built {
            Ok(provider) => providers.push(provider),
            Err(error) => failures.push(ProviderBuildFailure {
                provider: name.to_string(),
                error,
            }),
        }
    }
    (providers, failures)
}

/// `Ok(None)` when no key file is configured or the file does not exist; an
/// unreadable or empty file is an auth failure.
fn read_key(path: Option<&str>) -> Result<Option<String>, ProviderError> {
    let Some(path) = path else {
        return Ok(None);
    };
    let path = expand_path(path);
    let value = match std::fs::read_to_string(path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ProviderError::Auth),
    };
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(ProviderError::Auth);
    }
    Ok(Some(value))
}

fn expand_path(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        crate::utils::node_os::homedir().join(rest)
    } else {
        std::path::PathBuf::from(path)
    }
}
