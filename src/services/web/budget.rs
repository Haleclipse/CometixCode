//! Session-scoped WebSearch/WebFetch budgets.
//!
//! @nyan: counts live in the process-wide session owner so main-thread and
//! child-agent calls spend the same limits. `/clear` calls [`reset`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::utils::settings::nyan::{WebFetchSettings, WebSearchSettings};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BudgetError {
    SearchCalls { limit: u32 },
    FetchCalls { limit: u32 },
    FetchBytes { limit: u64 },
    ProviderCalls { provider: String, limit: u32 },
    ConcurrentDeadline,
}

impl std::fmt::Display for BudgetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SearchCalls { limit } => write!(
                f,
                "budget_exhausted: web search limit ({limit} per session) reached"
            ),
            Self::FetchCalls { limit } => write!(
                f,
                "budget_exhausted: web fetch limit ({limit} per session) reached"
            ),
            Self::FetchBytes { limit } => write!(
                f,
                "budget_exhausted: web fetch byte limit ({limit} per session) reached"
            ),
            Self::ProviderCalls { provider, limit } => write!(
                f,
                "budget_exhausted: {provider} provider limit ({limit} per session) reached"
            ),
            Self::ConcurrentDeadline => {
                f.write_str("budget_exhausted: web search concurrency deadline reached")
            }
        }
    }
}

impl std::error::Error for BudgetError {}

#[derive(Default)]
struct State {
    search_calls: u32,
    fetch_calls: u32,
    fetch_bytes: u64,
    provider_calls: HashMap<String, u32>,
    active_searches: u32,
}

struct SessionBudget {
    state: Mutex<State>,
    wake: tokio::sync::Notify,
}

fn shared() -> &'static Arc<SessionBudget> {
    static BUDGET: OnceLock<Arc<SessionBudget>> = OnceLock::new();
    BUDGET.get_or_init(|| {
        Arc::new(SessionBudget {
            state: Mutex::new(State::default()),
            wake: tokio::sync::Notify::new(),
        })
    })
}

pub struct SearchPermit {
    budget: Arc<SessionBudget>,
}

impl Drop for SearchPermit {
    fn drop(&mut self) {
        if let Ok(mut state) = self.budget.state.lock() {
            state.active_searches = state.active_searches.saturating_sub(1);
        }
        self.budget.wake.notify_one();
    }
}

pub struct FetchPermit;

pub fn begin_fetch(settings: &WebFetchSettings) -> Result<FetchPermit, BudgetError> {
    let budget = shared();
    let mut state = budget
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if state.fetch_calls >= settings.max_per_session() {
        return Err(BudgetError::FetchCalls {
            limit: settings.max_per_session(),
        });
    }
    state.fetch_calls += 1;
    Ok(FetchPermit)
}

pub fn add_fetch_bytes(settings: &WebFetchSettings, bytes: u64) -> Result<(), BudgetError> {
    let budget = shared();
    let mut state = budget
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let total = state.fetch_bytes.saturating_add(bytes);
    if total > settings.max_bytes_per_session() {
        return Err(BudgetError::FetchBytes {
            limit: settings.max_bytes_per_session(),
        });
    }
    state.fetch_bytes = total;
    Ok(())
}

pub async fn begin_search(
    settings: &WebSearchSettings,
    deadline: Duration,
) -> Result<SearchPermit, BudgetError> {
    let budget = Arc::clone(shared());
    {
        let mut state = budget
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.search_calls >= settings.max_per_session() {
            return Err(BudgetError::SearchCalls {
                limit: settings.max_per_session(),
            });
        }
        state.search_calls += 1;
    }

    let acquire = async {
        loop {
            let notified = budget.wake.notified();
            let available = {
                let mut state = budget
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if state.active_searches < settings.max_concurrent_searches() {
                    state.active_searches += 1;
                    true
                } else {
                    false
                }
            };
            if available {
                return Ok(SearchPermit {
                    budget: Arc::clone(&budget),
                });
            }
            notified.await;
        }
    };
    tokio::time::timeout(deadline, acquire)
        .await
        .map_err(|_| BudgetError::ConcurrentDeadline)?
}

pub fn reserve_provider(provider: &str, max_calls: Option<u32>) -> Result<(), BudgetError> {
    let Some(limit) = max_calls else {
        return Ok(());
    };
    let budget = shared();
    let mut state = budget
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let calls = state
        .provider_calls
        .entry(provider.to_string())
        .or_default();
    if *calls >= limit {
        return Err(BudgetError::ProviderCalls {
            provider: provider.to_string(),
            limit,
        });
    }
    *calls += 1;
    Ok(())
}

pub fn snapshot() -> (u32, u32, u64, HashMap<String, u32>) {
    let state = shared()
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    (
        state.search_calls,
        state.fetch_calls,
        state.fetch_bytes,
        state.provider_calls.clone(),
    )
}

pub fn reset() {
    let budget = shared();
    let mut state = budget
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *state = State::default();
    budget.wake.notify_waiters();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The session budget is process-wide, and the `search` / `fallback`
    /// suites drive real searches that charge it. Hold the same lock they do
    /// so a parallel search cannot move the counters underneath these asserts.
    fn test_lock() -> crate::utils::test_env::TestEnvGuard<'static> {
        let guard = crate::utils::test_env::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reset();
        guard
    }

    #[tokio::test]
    async fn counts_calls_bytes_and_provider_limits() {
        let _lock = test_lock();
        let mut search = WebSearchSettings::default();
        search.max_per_session = Some(1);
        let permit = begin_search(&search, Duration::from_secs(1)).await.unwrap();
        assert_eq!(snapshot().0, 1);
        drop(permit);
        assert!(matches!(
            begin_search(&search, Duration::from_secs(1)).await,
            Err(BudgetError::SearchCalls { limit: 1 })
        ));

        let mut fetch = WebFetchSettings::default();
        fetch.max_bytes_per_session = Some(4);
        begin_fetch(&fetch).unwrap();
        add_fetch_bytes(&fetch, 4).unwrap();
        assert!(matches!(
            add_fetch_bytes(&fetch, 1),
            Err(BudgetError::FetchBytes { limit: 4 })
        ));
        assert!(matches!(reserve_provider("exa", Some(1)), Ok(())));
        assert!(matches!(
            reserve_provider("exa", Some(1)),
            Err(BudgetError::ProviderCalls { .. })
        ));
    }

    #[tokio::test]
    async fn third_search_waits_for_a_permit() {
        let _lock = test_lock();
        let mut settings = WebSearchSettings::default();
        settings.max_concurrent_searches = Some(1);
        let first = begin_search(&settings, Duration::from_secs(1))
            .await
            .unwrap();
        let task = tokio::spawn(async move {
            let second = begin_search(&settings, Duration::from_secs(1))
                .await
                .unwrap();
            drop(second);
        });
        tokio::task::yield_now().await;
        drop(first);
        task.await.unwrap();
    }
}
