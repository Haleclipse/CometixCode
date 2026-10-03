//! Maps to: CC `utils/plugins/installCounts.ts`.
//! Partial native boundaries: chrono accepts RFC3339/RFC2822/date-only, not
//! Bun's complete implementation-defined Date grammar. Other accepted Bun
//! timestamps can therefore cause a cache miss here. Reqwest native transport
//! diagnostics/proxy/TLS behavior are not a complete Axios transport port.

use super::{
    fetch_telemetry::{
        PluginFetchOutcome, PluginFetchSource, classify_fetch_error, log_plugin_fetch,
    },
    plugin_directories::get_plugins_directory,
};
use crate::utils::{
    debug::log_for_debugging,
    json::JsoncValue,
    slow_operations::{json_parse, json_stringify},
};
use std::path::PathBuf;

const INSTALL_COUNTS_CACHE_VERSION: f64 = 1.0;
const INSTALL_COUNTS_CACHE_FILENAME: &str = "install-counts-cache.json";
const INSTALL_COUNTS_URL: &str = "https://raw.githubusercontent.com/anthropics/claude-plugins-official/refs/heads/stats/stats/plugin-installs.json";
const CACHE_TTL_MS: i64 = 24 * 60 * 60 * 1000;

/// Maps to: CC `utils/plugins/installCounts.ts:32-39#InstallCountsCache`.
/// Counts remain dynamic because the fetch branch does not validate entries.
struct InstallCountsCache {
    version: f64,
    fetched_at: String,
    counts: Vec<JsoncValue>,
}

/// Native carrier for CC `getInstallCounts`'s Map. The source network branch
/// can return undefined, non-number values and non-string keys despite its TS
/// annotation. Keep these until the actual consumer chooses a string lookup.
/// Distinct object keys from JSON nodes retain separate Map slots; JSON has no
/// shared object references. Scalar keys implement SameValueZero.
#[derive(Clone, Default)]
pub struct InstallCounts {
    entries: Vec<(Option<JsoncValue>, Option<JsoncValue>)>,
}

impl InstallCounts {
    fn set(&mut self, key: Option<JsoncValue>, value: Option<JsoncValue>) {
        let slot =
            self.entries
                .iter_mut()
                .find(|(existing, _)| match (existing.as_ref(), key.as_ref()) {
                    (None, None) => true,
                    (Some(a), Some(b)) if a.kind == b.kind => match a.kind {
                        7..=9 => true,
                        10 => a.string_units == b.string_units,
                        11 => {
                            let a = a.number.unwrap();
                            let b = b.number.unwrap();
                            a == b || (a.is_nan() && b.is_nan())
                        }
                        _ => false,
                    },
                    _ => false,
                });
        if let Some((_, previous)) = slot {
            *previous = value;
        } else {
            self.entries.push((key, value));
        }
    }

    /// Source Map.get(string) lookup, preserving the raw value. None is source
    /// undefined; callers must apply source nullish/numeric semantics themselves.
    pub fn get(&self, plugin: &str) -> Option<&JsoncValue> {
        let units: Vec<_> = plugin.encode_utf16().collect();
        self.entries
            .iter()
            .find(|(key, _)| {
                key.as_ref()
                    .is_some_and(|key| key.kind == 10 && key.string_units == units)
            })
            .and_then(|(_, value)| value.as_ref())
    }
}

/// Maps to: CC `utils/plugins/installCounts.ts:54-56#getInstallCountsCachePath`.
fn get_install_counts_cache_path() -> PathBuf {
    get_plugins_directory().join(INSTALL_COUNTS_CACHE_FILENAME)
}

/// Maps to: CC `utils/plugins/installCounts.ts:62-143#loadInstallCountsCache`.
async fn load_install_counts_cache() -> Option<InstallCountsCache> {
    let cache_path = get_install_counts_cache_path();
    let loaded: anyhow::Result<Option<InstallCountsCache>> = async {
        let bytes = tokio::fs::read(cache_path).await?;
        let parsed = json_parse(&String::from_utf8_lossy(&bytes))?;
        let version = parsed.get_property("version");
        let fetched_at = parsed.get_property("fetchedAt");
        let counts = parsed.get_property("counts");
        if !matches!(parsed.kind, 1 | 3)
            || version.is_none()
            || fetched_at.is_none()
            || counts.is_none()
        {
            log_for_debugging("Install counts cache has invalid structure");
            return Ok(None);
        }
        let version = version.unwrap();
        if version.kind != 11 || version.number != Some(INSTALL_COUNTS_CACHE_VERSION) {
            log_for_debugging(&format!(
                "Install counts cache version mismatch (got {}, expected 1)",
                version
                    .number
                    .map(|number| ryu_js::Buffer::new().format(number).to_owned())
                    .unwrap_or_else(|| crate::utils::zod::js_string(&version.to_json()))
            ));
            return Ok(None);
        }
        let fetched_at = fetched_at.unwrap();
        let counts = counts.unwrap();
        if fetched_at.kind != 10 || counts.as_array().is_none() {
            log_for_debugging("Install counts cache has invalid structure");
            return Ok(None);
        }
        let timestamp = fetched_at.as_str().unwrap();
        // Native Date parser boundary; see module-level explicit partial.
        let fetched_ms = chrono::DateTime::parse_from_rfc3339(timestamp)
            .ok()
            .map(|date| date.timestamp_millis())
            .or_else(|| {
                chrono::DateTime::parse_from_rfc2822(timestamp)
                    .ok()
                    .map(|date| date.timestamp_millis())
            })
            .or_else(|| {
                chrono::NaiveDate::parse_from_str(timestamp, "%Y-%m-%d")
                    .ok()
                    .and_then(|date| date.and_hms_opt(0, 0, 0))
                    .map(|date| date.and_utc().timestamp_millis())
            });
        let Some(fetched_ms) = fetched_ms else {
            log_for_debugging("Install counts cache has invalid fetchedAt timestamp");
            return Ok(None);
        };
        let counts = counts.as_array().unwrap();
        if !counts.iter().all(|entry| {
            matches!(entry.kind, 1 | 3)
                && entry
                    .get_property("plugin")
                    .is_some_and(|value| value.kind == 10)
                && entry
                    .get_property("unique_installs")
                    .is_some_and(|value| value.kind == 11)
        }) {
            log_for_debugging("Install counts cache has malformed entries");
            return Ok(None);
        }
        let now = chrono::Utc::now().timestamp_millis();
        #[cfg(test)]
        let now = tests::CLOCK_OVERRIDE.lock().unwrap().unwrap_or(now);
        if now.saturating_sub(fetched_ms) > CACHE_TTL_MS {
            log_for_debugging("Install counts cache is stale (>24h old)");
            return Ok(None);
        }
        Ok(Some(InstallCountsCache {
            version: 1.0,
            fetched_at: timestamp.into(),
            counts: counts.to_vec(),
        }))
    }
    .await;
    match loaded {
        Ok(cache) => cache,
        Err(error) => {
            if crate::utils::errors::get_errno_code(&error) != Some("ENOENT") {
                log_for_debugging(&format!("Failed to load install counts cache: {error}"));
            }
            None
        }
    }
}

/// Maps to: CC `utils/plugins/installCounts.ts:149-181#saveInstallCountsCache`.
async fn save_install_counts_cache(cache: &InstallCountsCache) -> anyhow::Result<()> {
    let cache_path = get_install_counts_cache_path();
    let mut bytes = [0u8; 8];
    // Source randomBytes is before the catch: entropy failures reach the caller.
    getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let suffix: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let temp_path = PathBuf::from(format!("{}.{suffix}.tmp", cache_path.display()));
    let saved: anyhow::Result<()> = async {
        crate::utils::fs_operations::mkdir(&get_plugins_directory(), None).await?;
        let mut raw = JsoncValue::from_json(
            serde_json::json!({"version":cache.version,"fetchedAt":cache.fetched_at,"counts":[]}),
        );
        *raw.properties
            .iter_mut()
            .find(|(key, _)| key == &"counts".encode_utf16().collect::<Vec<_>>())
            .unwrap() = (
            "counts".encode_utf16().collect(),
            JsoncValue::array(cache.counts.clone()),
        );
        let content = json_stringify(&raw, 2);
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temp_path).await.map_err(|error| {
            let message =
                crate::utils::errors::format_native_file_error(&error, "open", Some(&temp_path));
            anyhow::Error::new(error).context(message)
        })?;
        tokio::io::AsyncWriteExt::write_all(&mut file, content.as_bytes()).await?;
        // writeFile closes before rename; flush waits for Tokio's pending write.
        tokio::io::AsyncWriteExt::flush(&mut file).await?;
        drop(file);
        tokio::fs::rename(&temp_path, &cache_path).await?;
        log_for_debugging("Install counts cache saved successfully");
        Ok(())
    }
    .await;
    if let Err(error) = saved {
        crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
        let _ = tokio::fs::remove_file(temp_path).await;
    }
    Ok(())
}

/// Maps to: CC `utils/plugins/installCounts.ts:184-217#fetchInstallCountsFromGitHub`.
async fn fetch_install_counts_from_git_hub() -> anyhow::Result<Vec<JsoncValue>> {
    log_for_debugging(&format!(
        "Fetching install counts from {INSTALL_COUNTS_URL}"
    ));
    let started = std::time::Instant::now();
    let result: anyhow::Result<Vec<JsoncValue>> = async {
        // CC `axios.get` (`installCounts.ts:191-193`), through the global
        // interceptor; `create_axios_instance` resolves the same transport.
        let client = crate::utils::proxy::create_axios_instance()?
            .timeout(std::time::Duration::from_millis(10000))
            .build()?;
        let url = INSTALL_COUNTS_URL.to_owned();
        #[cfg(test)]
        let url = tests::HTTP_OVERRIDE.lock().unwrap().clone().unwrap_or(url);
        let response = client.get(url).send().await.map_err(|error| {
            if error.is_timeout() {
                anyhow::anyhow!("timeout of 10000ms exceeded")
            } else {
                anyhow::Error::new(error)
            }
        })?;
        if !response.status().is_success() {
            anyhow::bail!(
                "Request failed with status code {}",
                response.status().as_u16()
            );
        }
        // Axios's utf8 Buffer response decoding strips exactly one BOM.
        // Read bytes to avoid a transport text decoder stripping another one.
        let bytes = response.bytes().await?;
        let text = String::from_utf8_lossy(&bytes);
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        // Axios's default transform keeps invalid JSON as text; the source
        // then throws its own response-schema error, not JSON.parse's error.
        let response = json_parse(text).unwrap_or_else(|_| JsoncValue::from_json(text.into()));
        response
            .get_property("plugins")
            .and_then(JsoncValue::as_array)
            .map(<[JsoncValue]>::to_vec)
            .ok_or_else(|| anyhow::anyhow!("Invalid response format from install counts API"))
    }
    .await;
    match &result {
        Ok(_) => log_plugin_fetch(
            PluginFetchSource::InstallCounts,
            Some(INSTALL_COUNTS_URL),
            PluginFetchOutcome::Success,
            started.elapsed().as_secs_f64() * 1000.0,
            None,
        ),
        Err(error) => log_plugin_fetch(
            PluginFetchSource::InstallCounts,
            Some(INSTALL_COUNTS_URL),
            PluginFetchOutcome::Failure,
            started.elapsed().as_secs_f64() * 1000.0,
            Some(classify_fetch_error(error)),
        ),
    }
    result
}

/// Maps to: CC `utils/plugins/installCounts.ts:225-260#getInstallCounts`.
pub async fn get_install_counts() -> Option<InstallCounts> {
    if let Some(cache) = load_install_counts_cache().await {
        log_for_debugging("Using cached install counts");
        log_plugin_fetch(
            PluginFetchSource::InstallCounts,
            Some(INSTALL_COUNTS_URL),
            PluginFetchOutcome::CacheHit,
            0.0,
            None,
        );
        let mut map = InstallCounts::default();
        for entry in cache.counts {
            map.set(
                entry.get_property("plugin").cloned(),
                entry.get_property("unique_installs").cloned(),
            );
        }
        return Some(map);
    }
    let result: anyhow::Result<InstallCounts> = async {
        let counts = fetch_install_counts_from_git_hub().await?;
        let new_cache = InstallCountsCache {
            version: INSTALL_COUNTS_CACHE_VERSION,
            fetched_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            counts,
        };
        save_install_counts_cache(&new_cache).await?;
        let mut map = InstallCounts::default();
        for entry in new_cache.counts {
            if entry.is_null() {
                anyhow::bail!("null is not an object (evaluating 'entry.plugin')");
            }
            map.set(
                entry.get_property("plugin").cloned(),
                entry.get_property("unique_installs").cloned(),
            );
        }
        Ok(map)
    }
    .await;
    match result {
        Ok(map) => Some(map),
        Err(error) => {
            crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
            log_for_debugging(&format!("Failed to fetch install counts: {error}"));
            None
        }
    }
}

/// Rust input carrier for the source's dynamically typed parameter. Most
/// callers hold numeric values; marketplace count JSON may contain any JS value.
/// This changes only representation, keeping one formatInstallCount algorithm.
pub enum InstallCountInput<'a> {
    Number(f64),
    Value(&'a JsoncValue),
}
impl From<f64> for InstallCountInput<'_> {
    fn from(value: f64) -> Self {
        Self::Number(value)
    }
}
impl<'a> From<&'a JsoncValue> for InstallCountInput<'a> {
    fn from(value: &'a JsoncValue) -> Self {
        Self::Value(value)
    }
}
/// Maps to: CC `utils/plugins/installCounts.ts:273-295#formatInstallCount`.
pub fn format_install_count<'a>(count: impl Into<InstallCountInput<'a>>) -> String {
    let count = count.into();
    let number = match &count {
        InstallCountInput::Number(number) => *number,
        InstallCountInput::Value(value) => value
            .to_number()
            .expect("Cannot convert object to primitive value"),
    };
    if number < 1000.0 {
        return match count {
            InstallCountInput::Number(number) => ryu_js::Buffer::new().format(number).to_owned(),
            InstallCountInput::Value(value) => {
                if value.is_null() {
                    "null".into()
                } else {
                    value
                        .array_string()
                        .expect("Cannot convert object to primitive value")
                }
            }
        };
    }
    let (scaled, suffix) = if number < 1000000.0 {
        (number / 1000.0, "K")
    } else {
        (number / 1000000.0, "M")
    };
    let mut buffer = ryu_js::Buffer::new();
    let formatted = buffer.format_to_fixed(scaled, 1);
    format!(
        "{}{suffix}",
        formatted.strip_suffix(".0").unwrap_or(formatted)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    #[test]
    fn format_install_count_dynamic_json_matches_official_bun() {
        let oracle:serde_json::Value=serde_json::from_str(include_str!("../../../tests/fixtures/oracles/plugin-ui-complete-0914/counts-oracle.json")).unwrap();
        for case in oracle.as_array().unwrap() {
            let value = JsoncValue::from_json(case["value"].clone());
            assert_eq!(
                format_install_count(if value.is_null() {
                    InstallCountInput::Number(0.0)
                } else {
                    InstallCountInput::Value(&value)
                }),
                case["text"].as_str().unwrap(),
                "count={}",
                case["value"]
            );
        }
    }

    use std::{
        io::{Read, Write},
        sync::Mutex,
    };
    pub(super) static CLOCK_OVERRIDE: Mutex<Option<i64>> = Mutex::new(None);
    pub(super) static HTTP_OVERRIDE: Mutex<Option<String>> = Mutex::new(None);

    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn new() -> std::io::Result<Self> {
            let path =
                std::env::temp_dir().join(format!("install-counts-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path)?;
            Ok(Self(path))
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct BoundaryReset;
    impl Drop for BoundaryReset {
        fn drop(&mut self) {
            *CLOCK_OVERRIDE.lock().unwrap() = None;
            *HTTP_OVERRIDE.lock().unwrap() = None;
        }
    }

    struct Server(Option<std::thread::JoinHandle<()>>);
    impl Server {
        fn join(mut self) -> std::thread::Result<()> {
            self.0.take().unwrap().join()
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            // Native sockets have deadlines below; unwind also joins rather
            // than leaving a blocked fixture thread in the nextest process.
            if let Some(thread) = self.0.take() {
                let _ = thread.join();
            }
        }
    }

    // Test-only native HTTP boundary. The production fetch/parse/save/Map chain
    // runs unchanged; each response is served once, without an external request.
    fn serve(responses: Vec<(u16, &'static str)>) -> Server {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        *HTTP_OVERRIDE.lock().unwrap() =
            Some(format!("http://{}/counts", listener.local_addr().unwrap()));
        Server(Some(std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            for (status, body) in responses {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && std::time::Instant::now() < deadline =>
                        {
                            std::thread::sleep(std::time::Duration::from_millis(5))
                        }
                        Err(error) => panic!("HTTP fixture accept deadline/error: {error}"),
                    }
                };
                // macOS accept inherits the listener's nonblocking mode.
                // Only accept polling is nonblocking; request I/O must wait
                // for the client under the explicit deadlines below.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut request = [0; 8192];
                let size = stream.read(&mut request).unwrap();
                assert!(String::from_utf8_lossy(&request[..size]).starts_with("GET /counts "));
                write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        })))
    }

    #[test]
    fn format_install_count_matches_official_bun_rounding() {
        for (n, expected) in [
            (-f64::INFINITY, "-Infinity"),
            (-1.0, "-1"),
            (-0.0, "0"),
            (0.25, "0.25"),
            (999.99, "999.99"),
            (1000.0, "1K"),
            (1050.0, "1.1K"),
            (1150.0, "1.1K"),
            (1250.0, "1.3K"),
            (999949.0, "999.9K"),
            (999950.0, "1000K"),
            (999999.0, "1000K"),
            (1000000.0, "1M"),
            (1250000.0, "1.3M"),
            (1e21, "1000000000000000M"),
            (1e27, "1e+21M"),
            (f64::INFINITY, "InfinityM"),
            (f64::NAN, "NaNM"),
        ] {
            assert_eq!(format_install_count(n), expected, "{n}");
        }
    }

    #[tokio::test]
    async fn install_counts_cache_matches_official_ttl_validation_and_raw_numbers() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let temp = TestDirectory::new().unwrap();
        let _env = EnvVarGuard::set(
            "CLAUDE_CODE_PLUGIN_CACHE_DIR",
            temp.path().to_str().unwrap(),
        );
        let _reset = BoundaryReset;
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-14T00:00:00Z")
            .unwrap()
            .timestamp_millis();
        *CLOCK_OVERRIDE.lock().unwrap() = Some(now);
        for (time, valid) in [
            ("2026-09-13T00:00:00Z", true),
            ("2026-09-12T23:59:59.999Z", false),
            ("2030-01-01", true),
            ("Mon, 14 Sep 2026 00:00:00 GMT", true),
            ("nonsense", false),
        ] {
            std::fs::write(get_install_counts_cache_path(), format!(r#"{{"version":1,"fetchedAt":"{time}","counts":[{{"plugin":"a","unique_installs":1e400}}]}}"#)).unwrap();
            let result = load_install_counts_cache().await;
            assert_eq!(result.is_some(), valid, "{time}");
            if let Some(cache) = result {
                assert_eq!(
                    cache.counts[0]
                        .get_property("unique_installs")
                        .unwrap()
                        .number,
                    Some(f64::INFINITY)
                );
            }
        }
        for invalid in [
            "null",
            "[]",
            "{}",
            r#"{"version":"1","fetchedAt":"2026-09-14","counts":[]}"#,
            r#"{"version":1,"fetchedAt":"2026-09-14","counts":[null]}"#,
            r#"{"version":1,"fetchedAt":"2026-09-14","counts":[{"plugin":"a","unique_installs":"3"}]}"#,
        ] {
            std::fs::write(get_install_counts_cache_path(), invalid).unwrap();
            assert!(load_install_counts_cache().await.is_none(), "{invalid}");
        }
        std::fs::write(
            get_install_counts_cache_path(),
            "\u{feff}{\"version\":1,\"fetchedAt\":\"2026-09-14\",\"counts\":[]}",
        )
        .unwrap();
        assert!(load_install_counts_cache().await.is_none());
        // Explicit native Date gap, not a Bun parity assertion: no local/overflow
        // parser is invented to accept these source-valid cache timestamps.
        for date in ["2026-09-14 00:00:00", "2026-09-31"] {
            std::fs::write(
                get_install_counts_cache_path(),
                format!(r#"{{"version":1,"fetchedAt":"{date}","counts":[]}}"#),
            )
            .unwrap();
            assert!(load_install_counts_cache().await.is_none());
        }
    }

    #[tokio::test]
    async fn install_counts_fetch_matches_official_save_before_map_and_cache_hit() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let temp = TestDirectory::new().unwrap();
        let _env = EnvVarGuard::set(
            "CLAUDE_CODE_PLUGIN_CACHE_DIR",
            temp.path().to_str().unwrap(),
        );
        let _reset = BoundaryReset;
        let bom_server = serve(vec![(200, "\u{feff}{\"plugins\":[]}")]);
        assert!(get_install_counts().await.unwrap().entries.is_empty());
        bom_server.join().unwrap();
        std::fs::remove_file(get_install_counts_cache_path()).unwrap();
        let server = serve(vec![(
            200,
            r#"{"plugins":[{"plugin":"b","unique_installs":1},{"plugin":"a","unique_installs":2},{"plugin":"b","unique_installs":3}]}"#,
        )]);
        let map = get_install_counts().await.unwrap();
        server.join().unwrap();
        assert_eq!(map.entries.len(), 2);
        assert_eq!(map.get("b").unwrap().number, Some(3.0));
        assert_eq!(map.entries[0].0.as_ref().unwrap().as_str(), Some("b"));
        assert_eq!(
            get_install_counts().await.unwrap().get("a").unwrap().number,
            Some(2.0)
        ); // server gone: cache only
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(get_install_counts_cache_path())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        std::fs::remove_file(get_install_counts_cache_path()).unwrap();
        let server = serve(vec![(200, r#"{"plugins":[null]}"#)]);
        assert!(get_install_counts().await.is_none());
        server.join().unwrap();
        let saved =
            json_parse(&std::fs::read_to_string(get_install_counts_cache_path()).unwrap()).unwrap();
        assert!(saved.get_property("counts").unwrap().as_array().unwrap()[0].is_null());
        // The next load rejects that persisted entry before retrying the API.
        let server = serve(vec![(
            200,
            r#"{"plugins":[{},42,"x",false,{"plugin":"raw","unique_installs":"12"}]}"#,
        )]);
        let map = get_install_counts().await.unwrap();
        server.join().unwrap();
        assert_eq!(map.entries.len(), 2);
        assert!(map.entries[0].0.is_none());
        assert_eq!(map.get("raw").unwrap().as_str(), Some("12"));
    }

    #[tokio::test]
    async fn install_counts_fetch_matches_official_write_failure_and_invalid_response() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let temp = TestDirectory::new().unwrap();
        let _env = EnvVarGuard::set(
            "CLAUDE_CODE_PLUGIN_CACHE_DIR",
            temp.path().to_str().unwrap(),
        );
        let _reset = BoundaryReset;
        // An existing directory at the final filename makes rename fail. The
        // successful fetched Map still returns, and the random temp is removed.
        std::fs::create_dir(get_install_counts_cache_path()).unwrap();
        let server = serve(vec![(
            200,
            r#"{"plugins":[{"plugin":"ok","unique_installs":7}]}"#,
        )]);
        assert_eq!(
            get_install_counts()
                .await
                .unwrap()
                .get("ok")
                .unwrap()
                .number,
            Some(7.0)
        );
        server.join().unwrap();
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
        std::fs::remove_dir(get_install_counts_cache_path()).unwrap();
        for (status, body) in [(200, "not JSON"), (200, r#"{"plugins":{}}"#), (403, "{}")] {
            let server = serve(vec![(status, body)]);
            assert!(get_install_counts().await.is_none());
            server.join().unwrap();
            assert!(!get_install_counts_cache_path().exists());
        }
    }

    #[tokio::test]
    async fn install_counts_matches_official_mkdir_eexist_then_open_enotdir() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let temp = TestDirectory::new().unwrap();
        let file = temp.path().join("ordinary-plugins-file");
        std::fs::write(&file, "unchanged").unwrap();
        let _env = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", file.to_str().unwrap());
        let _logging = [
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "DISABLE_ERROR_REPORTING",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
        ]
        .map(EnvVarGuard::unset);
        let _reset = BoundaryReset;
        let server = serve(vec![(
            200,
            r#"{"plugins":[{"plugin":"after-mkdir","unique_installs":9}]}"#,
        )]);
        let map = get_install_counts().await.unwrap();
        server.join().unwrap();
        assert_eq!(map.get("after-mkdir").unwrap().number, Some(9.0));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "unchanged");
        let errors = crate::utils::log::get_in_memory_errors();
        assert!(errors.iter().any(|entry| entry.error.contains("ENOTDIR")
            && entry.error.contains("open '")
            && entry.error.contains(file.to_str().unwrap())
            && entry.error.contains("install-counts-cache.json.")
            && entry.error.contains(".tmp")));
        assert!(
            !errors.iter().any(|entry| entry.error.contains("EEXIST")
                && entry.error.contains(file.to_str().unwrap()))
        );
    }

    #[tokio::test]
    async fn install_counts_concurrent_misses_match_official_independent_fetches() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let temp = TestDirectory::new().unwrap();
        let _env = EnvVarGuard::set(
            "CLAUDE_CODE_PLUGIN_CACHE_DIR",
            temp.path().to_str().unwrap(),
        );
        let _reset = BoundaryReset;
        let server = serve(vec![
            (200, r#"{"plugins":[{"plugin":"a","unique_installs":1}]}"#),
            (200, r#"{"plugins":[{"plugin":"a","unique_installs":2}]}"#),
        ]);
        let (a, b) = tokio::join!(get_install_counts(), get_install_counts());
        server.join().unwrap();
        let mut counts = [
            a.unwrap().get("a").unwrap().number.unwrap(),
            b.unwrap().get("a").unwrap().number.unwrap(),
        ];
        counts.sort_by(f64::total_cmp);
        assert_eq!(counts, [1.0, 2.0]);
        let cache = load_install_counts_cache().await.unwrap();
        assert!(
            [1.0, 2.0].contains(
                &cache.counts[0]
                    .get_property("unique_installs")
                    .unwrap()
                    .number
                    .unwrap()
            )
        );
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }
}
