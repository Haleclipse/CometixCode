//! Live Claude.ai quota snapshot.
//!
//! Maps to: CC `services/claudeAiLimits.ts`.
//!
//! The API layer owns updates from successful response headers and 429 error
//! headers. Startup owns the one-token quota preflight, and the interactive
//! root bridges listener changes into AppState; no network or configuration I/O
//! occurs on retained TUI frames. Analytics remains intentionally omitted.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, LazyLock, Mutex, RwLock};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum QuotaStatus {
    #[default]
    Allowed,
    AllowedWarning,
    Rejected,
}

impl QuotaStatus {
    pub fn from_header(value: Option<&str>) -> Self {
        match value {
            Some("allowed_warning") => Self::AllowedWarning,
            Some("rejected") => Self::Rejected,
            _ => Self::Allowed,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::AllowedWarning => "allowed_warning",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateLimitType {
    FiveHour,
    SevenDay,
    SevenDayOpus,
    SevenDaySonnet,
    Overage,
}

impl RateLimitType {
    fn from_header(value: &str) -> Option<Self> {
        match value {
            "five_hour" => Some(Self::FiveHour),
            "seven_day" => Some(Self::SevenDay),
            "seven_day_opus" => Some(Self::SevenDayOpus),
            "seven_day_sonnet" => Some(Self::SevenDaySonnet),
            "overage" => Some(Self::Overage),
            _ => None,
        }
    }
}

/// Maps to CC `ClaudeAILimits`.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaudeAiLimits {
    pub status: QuotaStatus,
    pub unified_rate_limit_fallback_available: bool,
    pub is_using_overage: bool,
    pub resets_at: Option<f64>,
    pub rate_limit_type: Option<RateLimitType>,
    pub utilization: Option<f64>,
    pub overage_status: Option<QuotaStatus>,
    pub overage_resets_at: Option<f64>,
    pub overage_disabled_reason: Option<String>,
    pub surpassed_threshold: Option<f64>,
}

impl Default for ClaudeAiLimits {
    fn default() -> Self {
        Self {
            status: QuotaStatus::Allowed,
            unified_rate_limit_fallback_available: false,
            is_using_overage: false,
            resets_at: None,
            rate_limit_type: None,
            utilization: None,
            overage_status: None,
            overage_resets_at: None,
            overage_disabled_reason: None,
            surpassed_threshold: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawWindowUtilization {
    pub utilization: f64,
    pub resets_at: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawUtilization {
    pub five_hour: Option<RawWindowUtilization>,
    pub seven_day: Option<RawWindowUtilization>,
}

type StatusListener = Arc<dyn Fn(Arc<ClaudeAiLimits>) + Send + Sync>;

static CURRENT_LIMITS: LazyLock<RwLock<Arc<ClaudeAiLimits>>> =
    LazyLock::new(|| RwLock::new(Arc::new(ClaudeAiLimits::default())));
static RAW_UTILIZATION: LazyLock<RwLock<RawUtilization>> =
    LazyLock::new(|| RwLock::new(RawUtilization::default()));
static STATUS_LISTENERS: LazyLock<Mutex<BTreeMap<u64, StatusListener>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));
static NEXT_LISTENER_ID: LazyLock<std::sync::atomic::AtomicU64> =
    LazyLock::new(|| std::sync::atomic::AtomicU64::new(1));

pub fn current_limits() -> Arc<ClaudeAiLimits> {
    CURRENT_LIMITS
        .read()
        .expect("Claude.ai limits lock poisoned")
        .clone()
}

pub fn raw_utilization() -> RawUtilization {
    RAW_UTILIZATION
        .read()
        .expect("Claude.ai utilization lock poisoned")
        .clone()
}

pub fn subscribe(listener: impl Fn(Arc<ClaudeAiLimits>) + Send + Sync + 'static) -> u64 {
    let id = NEXT_LISTENER_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    STATUS_LISTENERS
        .lock()
        .expect("Claude.ai limits listener lock poisoned")
        .insert(id, Arc::new(listener));
    id
}

pub fn unsubscribe(id: u64) {
    STATUS_LISTENERS
        .lock()
        .expect("Claude.ai limits listener lock poisoned")
        .remove(&id);
}

fn emit_status_change(limits: ClaudeAiLimits) {
    let limits = Arc::new(limits);
    {
        let mut current = CURRENT_LIMITS
            .write()
            .expect("Claude.ai limits lock poisoned");
        if **current == *limits {
            return;
        }
        *current = limits.clone();
    }
    let listeners = STATUS_LISTENERS
        .lock()
        .expect("Claude.ai limits listener lock poisoned")
        .values()
        .cloned()
        .collect::<Vec<_>>();
    for listener in listeners {
        listener(limits.clone());
    }
}

fn header<'a>(headers: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .or_else(|| {
            headers
                .iter()
                .find_map(|(key, value)| key.eq_ignore_ascii_case(name).then_some(value))
        })
        .map(String::as_str)
}

fn number_header(headers: &HashMap<String, String>, name: &str) -> Option<f64> {
    header(headers, name)?.parse::<f64>().ok()
}

fn extract_raw_utilization(headers: &HashMap<String, String>) -> RawUtilization {
    fn window(
        headers: &HashMap<String, String>,
        abbreviation: &str,
    ) -> Option<RawWindowUtilization> {
        Some(RawWindowUtilization {
            utilization: number_header(
                headers,
                &format!("anthropic-ratelimit-unified-{abbreviation}-utilization"),
            )?,
            resets_at: number_header(
                headers,
                &format!("anthropic-ratelimit-unified-{abbreviation}-reset"),
            )?,
        })
    }
    RawUtilization {
        five_hour: window(headers, "5h"),
        seven_day: window(headers, "7d"),
    }
}

fn compute_time_progress(resets_at: f64, window_seconds: f64, now_seconds: f64) -> f64 {
    let window_start = resets_at - window_seconds;
    ((now_seconds - window_start) / window_seconds).clamp(0.0, 1.0)
}

fn early_warning(
    headers: &HashMap<String, String>,
    fallback_available: bool,
    now_seconds: f64,
) -> Option<ClaudeAiLimits> {
    for (abbreviation, rate_limit_type) in [
        ("5h", RateLimitType::FiveHour),
        ("7d", RateLimitType::SevenDay),
        ("overage", RateLimitType::Overage),
    ] {
        let threshold_name =
            format!("anthropic-ratelimit-unified-{abbreviation}-surpassed-threshold");
        let Some(surpassed_threshold) = number_header(headers, &threshold_name) else {
            continue;
        };
        return Some(ClaudeAiLimits {
            status: QuotaStatus::AllowedWarning,
            resets_at: number_header(
                headers,
                &format!("anthropic-ratelimit-unified-{abbreviation}-reset"),
            ),
            rate_limit_type: Some(rate_limit_type),
            utilization: number_header(
                headers,
                &format!("anthropic-ratelimit-unified-{abbreviation}-utilization"),
            ),
            unified_rate_limit_fallback_available: fallback_available,
            surpassed_threshold: Some(surpassed_threshold),
            ..ClaudeAiLimits::default()
        });
    }

    for (abbreviation, rate_limit_type, window_seconds, thresholds) in [
        (
            "5h",
            RateLimitType::FiveHour,
            5.0 * 60.0 * 60.0,
            &[(0.9, 0.72)][..],
        ),
        (
            "7d",
            RateLimitType::SevenDay,
            7.0 * 24.0 * 60.0 * 60.0,
            &[(0.75, 0.6), (0.5, 0.35), (0.25, 0.15)][..],
        ),
    ] {
        let utilization = number_header(
            headers,
            &format!("anthropic-ratelimit-unified-{abbreviation}-utilization"),
        );
        let resets_at = number_header(
            headers,
            &format!("anthropic-ratelimit-unified-{abbreviation}-reset"),
        );
        let (Some(utilization), Some(resets_at)) = (utilization, resets_at) else {
            continue;
        };
        let time_progress = compute_time_progress(resets_at, window_seconds, now_seconds);
        if thresholds.iter().any(|(minimum_usage, maximum_time)| {
            utilization >= *minimum_usage && time_progress <= *maximum_time
        }) {
            return Some(ClaudeAiLimits {
                status: QuotaStatus::AllowedWarning,
                resets_at: Some(resets_at),
                rate_limit_type: Some(rate_limit_type),
                utilization: Some(utilization),
                unified_rate_limit_fallback_available: fallback_available,
                ..ClaudeAiLimits::default()
            });
        }
    }
    None
}

fn compute_new_limits_from_headers(
    headers: &HashMap<String, String>,
    now_seconds: f64,
) -> ClaudeAiLimits {
    let status = QuotaStatus::from_header(header(headers, "anthropic-ratelimit-unified-status"));
    let fallback_available =
        header(headers, "anthropic-ratelimit-unified-fallback") == Some("available");

    if matches!(status, QuotaStatus::Allowed | QuotaStatus::AllowedWarning) {
        if let Some(warning) = early_warning(headers, fallback_available, now_seconds) {
            return warning;
        }
    }

    let overage_status = header(headers, "anthropic-ratelimit-unified-overage-status")
        .map(|value| QuotaStatus::from_header(Some(value)));
    ClaudeAiLimits {
        status: if matches!(status, QuotaStatus::Allowed | QuotaStatus::AllowedWarning) {
            QuotaStatus::Allowed
        } else {
            status
        },
        resets_at: number_header(headers, "anthropic-ratelimit-unified-reset"),
        unified_rate_limit_fallback_available: fallback_available,
        rate_limit_type: header(headers, "anthropic-ratelimit-unified-representative-claim")
            .and_then(RateLimitType::from_header),
        overage_status,
        overage_resets_at: number_header(headers, "anthropic-ratelimit-unified-overage-reset"),
        overage_disabled_reason: header(
            headers,
            "anthropic-ratelimit-unified-overage-disabled-reason",
        )
        .map(ToOwned::to_owned),
        is_using_overage: status == QuotaStatus::Rejected
            && matches!(
                overage_status,
                Some(QuotaStatus::Allowed | QuotaStatus::AllowedWarning)
            ),
        utilization: None,
        surpassed_threshold: None,
    }
}

/// Header-only projection used by the 429 message formatter. Unlike the live
/// service update, this mirrors CC `errors.ts` by treating the rejected request
/// as non-overage UI state even when an overage header is present.
pub fn limits_from_error_headers(headers: &HashMap<String, String>) -> ClaudeAiLimits {
    let headers = crate::services::rate_limit_mocking::process_rate_limit_headers(headers);
    let mut limits = compute_new_limits_from_headers(
        &headers,
        chrono::Utc::now().timestamp_millis() as f64 / 1000.0,
    );
    limits.status = QuotaStatus::Rejected;
    limits.is_using_overage = false;
    limits
}

fn should_process_rate_limits() -> bool {
    crate::services::rate_limit_mocking::should_process_rate_limits(
        crate::utils::auth::is_claude_ai_subscriber(),
    )
}

fn cache_extra_usage_disabled_reason(headers: &HashMap<String, String>) {
    // CC `claudeAiLimits.ts:443-444` coalesces the missing header to `null`, so
    // an observed response never leaves the cache in the "no cache yet" state.
    let reason = crate::utils::config::CachedExtraUsageDisabledReason::from_header(header(
        headers,
        "anthropic-ratelimit-unified-overage-disabled-reason",
    ));
    let current = crate::utils::config::load_global_config().cached_extra_usage_disabled_reason;
    if current != reason {
        if let Err(error) = crate::utils::config::save_global_config(|config| {
            config.cached_extra_usage_disabled_reason = reason;
        }) {
            crate::utils::debug::log_for_debugging(&format!(
                "Failed to cache extra usage disabled reason: {error}"
            ));
        }
    }
}

/// Maps to CC `extractQuotaStatusFromHeaders`.
pub fn extract_quota_status_from_headers(headers: &HashMap<String, String>) {
    let headers = crate::services::rate_limit_mocking::process_rate_limit_headers(headers);
    let should_process = should_process_rate_limits();
    if should_process {
        cache_extra_usage_disabled_reason(&headers);
    }
    extract_quota_status_from_headers_with(
        &headers,
        should_process,
        chrono::Utc::now().timestamp_millis() as f64 / 1000.0,
    );
}

pub fn extract_quota_status_from_headers_with(
    headers: &HashMap<String, String>,
    should_process: bool,
    now_seconds: f64,
) {
    if !should_process {
        *RAW_UTILIZATION
            .write()
            .expect("Claude.ai utilization lock poisoned") = RawUtilization::default();
        emit_status_change(ClaudeAiLimits::default());
        return;
    }

    *RAW_UTILIZATION
        .write()
        .expect("Claude.ai utilization lock poisoned") = extract_raw_utilization(headers);
    emit_status_change(compute_new_limits_from_headers(headers, now_seconds));
}

/// Maps to CC `extractQuotaStatusFromError`.
pub fn extract_quota_status_from_error(
    status: Option<u16>,
    headers: Option<&HashMap<String, String>>,
) {
    let processed_headers =
        headers.map(crate::services::rate_limit_mocking::process_rate_limit_headers);
    let should_process = should_process_rate_limits();
    if should_process {
        if let Some(headers) = processed_headers.as_ref() {
            cache_extra_usage_disabled_reason(headers);
        }
    }
    extract_quota_status_from_error_with(
        status,
        processed_headers.as_ref(),
        should_process,
        chrono::Utc::now().timestamp_millis() as f64 / 1000.0,
    );
}

pub fn extract_quota_status_from_error_with(
    status: Option<u16>,
    headers: Option<&HashMap<String, String>>,
    should_process: bool,
    now_seconds: f64,
) {
    if !should_process || status != Some(429) {
        return;
    }
    let mut limits = headers
        .map(|headers| {
            *RAW_UTILIZATION
                .write()
                .expect("Claude.ai utilization lock poisoned") = extract_raw_utilization(headers);
            compute_new_limits_from_headers(headers, now_seconds)
        })
        .unwrap_or_else(|| (*current_limits()).clone());
    limits.status = QuotaStatus::Rejected;
    emit_status_change(limits);
}

/// Pure startup gate for CC `checkQuotaStatus()`.
pub fn should_check_quota_status_with(
    essential_traffic_only: bool,
    should_process: bool,
    non_interactive: bool,
    bare: bool,
) -> bool {
    !essential_traffic_only && should_process && !non_interactive && !bare
}

#[cfg(test)]
fn quota_test_query_body(model: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "max_tokens": 1,
        "messages": [{"role": "user", "content": "quota"}],
        "metadata": crate::services::api::claude::get_api_metadata(),
    })
}

/// Maps to CC `services/claudeAiLimits.ts:194-248` `checkQuotaStatus()`.
///
/// This is startup/background I/O. It must never be called from an iocraft
/// retained frame; [`crate::main`] spawns it only after setup/trust completes.
pub async fn check_quota_status() {
    let bare = crate::utils::env_utils::is_bare_mode();
    if !should_check_quota_status_with(
        crate::utils::privacy_level::is_essential_traffic_only(),
        should_process_rate_limits(),
        crate::bootstrap::state::get_is_non_interactive_session(),
        bare,
    ) {
        return;
    }

    let model = crate::utils::model::model::get_small_fast_model();
    let client = match crate::services::api::client::get_anthropic_client(
        crate::services::api::client::GetAnthropicClientOptions {
            api_key: None,
            max_retries: 0,
            model: Some(model.clone()),
            source: Some("quota_check".to_string()),
        },
    )
    .await
    .and_then(|handle| handle.build().map_err(anyhow::Error::new))
    {
        Ok(client) => client,
        Err(error) => {
            crate::utils::debug::log_for_debugging(&format!(
                "Quota status preflight client setup failed: {error}"
            ));
            return;
        }
    };

    match make_test_query(&client, &model).await {
        Ok(response) => extract_quota_status_from_headers(&response.response.headers),
        Err(error) => {
            extract_quota_status_from_error(error.status(), error.headers());
        }
    }
}

/// Maps to CC `services/claudeAiLimits.ts:199-218` `makeTestQuery()`.
async fn make_test_query(
    client: &crate::services::api::client::ClientBuildOutput,
    model: &str,
) -> Result<
    anthropic_sdk::core::response::ApiResponse<
        anthropic_sdk::resources::beta::messages::BetaMessage,
    >,
    anthropic_sdk::ApiError,
> {
    let betas = crate::utils::betas::get_model_betas(model);
    let params = anthropic_sdk::resources::beta::messages::BetaMessageCreateParams {
        model: model.to_string(),
        max_tokens: 1,
        messages: vec![anthropic_sdk::resources::beta::messages::BetaMessageParam {
            role: "user".to_string(),
            content: anthropic_sdk::resources::beta::messages::BetaMessageContent::Text(
                "quota".to_string(),
            ),
        }],
        metadata: Some(anthropic_sdk::resources::messages::Metadata {
            user_id: Some(crate::services::api::claude::get_api_metadata().user_id),
        }),
        betas: (!betas.is_empty()).then_some(betas),
        ..Default::default()
    };
    client
        .beta()
        .messages()
        .create_with_response_and_options(&params, None)
        .await
}

/// Starts the preflight without putting network I/O on the retained frame.
pub fn spawn_quota_status_preflight() {
    #[cfg(not(test))]
    tokio::spawn(check_quota_status());
}

#[derive(Default)]
struct LimitsUiState {
    shown_warning: Option<String>,
    has_shown_overage_notification: bool,
}

fn apply_limits_to_store(
    store: &crate::state::store::AppStore,
    limits: Arc<ClaudeAiLimits>,
    context: &crate::services::rate_limit_messages::RateLimitUiContext,
    ui_state: &Mutex<LimitsUiState>,
) {
    let mut warning_notification = None;
    let mut overage_notification = None;
    if !context.is_remote_mode {
        let warning =
            crate::services::rate_limit_messages::get_rate_limit_warning(&limits, context);
        let mut ui_state = ui_state.lock().expect("limits UI state lock poisoned");
        warning_notification =
            crate::hooks::notifs::rate_limit_warning::rate_limit_warning_notification(
                warning.as_deref(),
                ui_state.shown_warning.as_deref(),
            );
        if let Some(warning) = warning {
            if warning_notification.is_some() {
                ui_state.shown_warning = Some(warning);
            }
        }
        overage_notification = crate::hooks::notifs::rate_limit_warning::overage_mode_notification(
            limits.is_using_overage,
            ui_state.has_shown_overage_notification,
            matches!(
                context.subscription_type.as_deref(),
                Some("team" | "enterprise")
            ),
            context.has_billing_access,
            crate::services::rate_limit_messages::get_using_overage_text(&limits, context),
        );
        if limits.is_using_overage && overage_notification.is_some() {
            ui_state.has_shown_overage_notification = true;
        } else if !limits.is_using_overage {
            ui_state.has_shown_overage_notification = false;
        }
    }

    store.replace_with(|state| {
        state.claude_ai_limits = limits.clone();
    });

    // CC `useRateLimitWarningNotification` emits through `addNotification`
    // (context/notifications.tsx:198-252), which is the mutation `setAppState`
    // PLUS the trailing `processQueue()` that promotes the head of the queue
    // into `current`. Writing `NotificationsState::add` straight into a
    // `replace_with` skips the promote entirely, so the warning would sit in
    // the queue forever and never reach the footer.
    let mut notifications = crate::context::notifications::NotificationsWriter::new(store.clone());
    if let Some(notification) = warning_notification {
        notifications.add_notification(notification);
    }
    if let Some(notification) = overage_notification {
        notifications.add_notification(notification);
    }
}

/// RAII bridge for CC `useClaudeAiLimits()` plus
/// `useRateLimitWarningNotification()`.
///
/// The interactive launch owner keeps one instance alive for the whole App
/// mount. Dropping it unregisters the global service listener, matching the
/// React hook cleanup function.
pub struct ClaudeAiLimitsStoreSubscription {
    listener_id: Option<u64>,
}

impl Drop for ClaudeAiLimitsStoreSubscription {
    fn drop(&mut self) {
        if let Some(listener_id) = self.listener_id.take() {
            unsubscribe(listener_id);
        }
    }
}

fn bind_app_store_with_context_provider(
    store: crate::state::store::AppStore,
    context_provider: Arc<
        dyn Fn() -> crate::services::rate_limit_messages::RateLimitUiContext + Send + Sync,
    >,
) -> ClaudeAiLimitsStoreSubscription {
    let ui_state = Arc::new(Mutex::new(LimitsUiState::default()));
    let listener_store = store.clone();
    let listener_context_provider = context_provider.clone();
    let listener_ui_state = ui_state.clone();
    let listener_id = subscribe(move |limits| {
        // Status changes originate from API/preflight workers. Refreshing the
        // auth snapshot here observes onboarding completed after launch without
        // moving config I/O onto retained rendering.
        let context = listener_context_provider();
        apply_limits_to_store(&listener_store, limits, &context, &listener_ui_state);
    });
    // Subscribe before the initial read so a concurrent response update cannot
    // fall between snapshot projection and listener registration.
    apply_limits_to_store(&store, current_limits(), &context_provider(), &ui_state);
    ClaudeAiLimitsStoreSubscription {
        listener_id: Some(listener_id),
    }
}

pub fn bind_app_store(store: crate::state::store::AppStore) -> ClaudeAiLimitsStoreSubscription {
    bind_app_store_with_context_provider(
        store,
        Arc::new(|| {
            crate::services::rate_limit_messages::RateLimitUiContext::from_global_config(
                &crate::utils::config::load_global_config(),
            )
        }),
    )
}

#[cfg(test)]
fn bind_app_store_with_context(
    store: crate::state::store::AppStore,
    context: crate::services::rate_limit_messages::RateLimitUiContext,
) -> ClaudeAiLimitsStoreSubscription {
    bind_app_store_with_context_provider(store, Arc::new(move || context.clone()))
}

#[cfg(test)]
pub fn reset_for_test() {
    *CURRENT_LIMITS
        .write()
        .expect("Claude.ai limits lock poisoned") = Arc::new(ClaudeAiLimits::default());
    *RAW_UTILIZATION
        .write()
        .expect("Claude.ai utilization lock poisoned") = RawUtilization::default();
    STATUS_LISTENERS
        .lock()
        .expect("Claude.ai limits listener lock poisoned")
        .clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    fn headers(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn header_status_and_overage_projection_match_official() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        reset_for_test();
        extract_quota_status_from_headers_with(
            &headers(&[
                ("anthropic-ratelimit-unified-status", "rejected"),
                ("anthropic-ratelimit-unified-reset", "123"),
                ("anthropic-ratelimit-unified-overage-status", "allowed"),
                ("anthropic-ratelimit-unified-overage-reset", "456"),
                (
                    "anthropic-ratelimit-unified-representative-claim",
                    "seven_day",
                ),
            ]),
            true,
            0.0,
        );
        let limits = current_limits();
        assert_eq!(limits.status, QuotaStatus::Rejected);
        assert_eq!(limits.resets_at, Some(123.0));
        assert_eq!(limits.rate_limit_type, Some(RateLimitType::SevenDay));
        assert!(limits.is_using_overage);
    }

    #[test]
    fn surpassed_threshold_and_time_relative_warning_match_official() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        reset_for_test();
        extract_quota_status_from_headers_with(
            &headers(&[
                ("anthropic-ratelimit-unified-status", "allowed"),
                ("anthropic-ratelimit-unified-7d-utilization", "0.5"),
                ("anthropic-ratelimit-unified-7d-reset", "604800"),
                ("anthropic-ratelimit-unified-7d-surpassed-threshold", "0.5"),
            ]),
            true,
            100.0,
        );
        let limits = current_limits();
        assert_eq!(limits.status, QuotaStatus::AllowedWarning);
        assert_eq!(limits.rate_limit_type, Some(RateLimitType::SevenDay));
        assert_eq!(limits.surpassed_threshold, Some(0.5));

        extract_quota_status_from_headers_with(
            &headers(&[
                ("anthropic-ratelimit-unified-status", "allowed"),
                ("anthropic-ratelimit-unified-5h-utilization", "0.95"),
                ("anthropic-ratelimit-unified-5h-reset", "18000"),
            ]),
            true,
            1.0,
        );
        assert_eq!(current_limits().status, QuotaStatus::AllowedWarning);
        assert_eq!(raw_utilization().five_hour.unwrap().utilization, 0.95);
    }

    #[test]
    fn error_without_headers_forces_rejected_and_non_subscriber_resets() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        reset_for_test();
        extract_quota_status_from_error_with(Some(429), None, true, 0.0);
        assert_eq!(current_limits().status, QuotaStatus::Rejected);

        extract_quota_status_from_headers_with(&HashMap::new(), false, 0.0);
        assert_eq!(*current_limits(), ClaudeAiLimits::default());
    }

    #[test]
    fn listeners_only_receive_real_status_changes() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        reset_for_test();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_for_listener = calls.clone();
        let id = subscribe(move |_| {
            calls_for_listener.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        extract_quota_status_from_headers_with(&HashMap::new(), true, 0.0);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        extract_quota_status_from_error_with(Some(429), None, true, 0.0);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        unsubscribe(id);
    }

    #[test]
    fn quota_preflight_gate_matches_official_startup_skips() {
        assert!(should_check_quota_status_with(false, true, false, false));
        assert!(!should_check_quota_status_with(true, true, false, false));
        assert!(!should_check_quota_status_with(false, false, false, false));
        assert!(!should_check_quota_status_with(false, true, true, false));
        assert!(!should_check_quota_status_with(false, true, false, true));

        let body = quota_test_query_body("claude-haiku-test");
        assert_eq!(body["model"], "claude-haiku-test");
        assert_eq!(body["max_tokens"], 1);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "quota");
    }

    #[test]
    fn overage_disabled_reason_uses_official_persisted_config_key() {
        use crate::utils::config::CachedExtraUsageDisabledReason;

        let config = crate::utils::config::GlobalConfig {
            cached_extra_usage_disabled_reason: CachedExtraUsageDisabledReason::Disabled(
                "out_of_credits".to_string(),
            ),
            ..Default::default()
        };
        let value = serde_json::to_value(config).unwrap();
        assert_eq!(value["cachedExtraUsageDisabledReason"], "out_of_credits");

        // CC `claudeAiLimits.ts:443-444` writes `header ?? null`, so a response
        // without the header caches the "enabled" state, not "no cache yet".
        assert_eq!(
            CachedExtraUsageDisabledReason::from_header(None),
            CachedExtraUsageDisabledReason::Enabled
        );
        assert_eq!(
            CachedExtraUsageDisabledReason::from_header(Some("org_level_disabled")),
            CachedExtraUsageDisabledReason::Disabled("org_level_disabled".to_string())
        );
    }

    #[test]
    fn app_store_bridge_updates_live_limits_notifications_and_cleans_up() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        reset_for_test();
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let subscription = bind_app_store_with_context(
            store.clone(),
            crate::services::rate_limit_messages::RateLimitUiContext {
                subscription_type: Some("max".to_string()),
                has_billing_access: true,
                ..Default::default()
            },
        );
        extract_quota_status_from_headers_with(
            &headers(&[
                ("anthropic-ratelimit-unified-status", "allowed"),
                ("anthropic-ratelimit-unified-7d-utilization", "0.8"),
                ("anthropic-ratelimit-unified-7d-reset", "604800"),
                ("anthropic-ratelimit-unified-7d-surpassed-threshold", "0.8"),
            ]),
            true,
            100.0,
        );
        let snapshot = store.get();
        assert_eq!(
            snapshot.claude_ai_limits.status,
            QuotaStatus::AllowedWarning
        );
        assert!(
            snapshot
                .notifications
                .current
                .as_ref()
                .is_some_and(|notification| notification.key == "rate-limit-warning")
        );

        drop(subscription);
        extract_quota_status_from_error_with(Some(429), None, true, 100.0);
        assert_eq!(
            store.get().claude_ai_limits.status,
            QuotaStatus::AllowedWarning,
            "dropped hook must unsubscribe from statusListeners"
        );
        reset_for_test();
    }
}
