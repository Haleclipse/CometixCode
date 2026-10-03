//! Plan file path helpers.
//!
//! Maps to CC `utils/plans.ts`. Path/slug helpers and fork plan copying live
//! here, along with source-owned resume recovery and remote file snapshots.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::MutexGuard;
use std::sync::{LazyLock, RwLock};

use crate::utils::settings::types::SettingsJson;
#[cfg(test)]
use crate::utils::test_env::TestStateLock;

const MAX_SLUG_RETRIES: usize = 10;

/// Maps to: CC `utils/plans.ts#getPlansDirectory` memoized resolver cache.
static PLANS_DIRECTORY_CACHE: LazyLock<RwLock<Option<PathBuf>>> =
    LazyLock::new(|| RwLock::new(None));

/// Rust-owned equivalent of CC `bootstrap/state.ts` `planSlugCache`.
static PLAN_SLUG_CACHE: LazyLock<RwLock<HashMap<String, String>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

#[cfg(test)]
static PLAN_TEST_LOCK: LazyLock<TestStateLock> = LazyLock::new(TestStateLock::new);

#[cfg(test)]
pub(crate) fn test_plan_state_lock() -> MutexGuard<'static, ()> {
    let guard = PLAN_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    clear_plans_directory_cache();
    guard
}

/// Maps to CC `utils/plans.ts` `getPlanSlug(...)`.
pub fn get_plan_slug(session_id: Option<&str>) -> String {
    let session_id = session_id
        .map(str::to_string)
        .unwrap_or_else(crate::bootstrap::state::get_session_id);

    if let Ok(cache) = PLAN_SLUG_CACHE.read() {
        if let Some(slug) = cache.get(&session_id).filter(|slug| !slug.is_empty()) {
            return slug.clone();
        }
    }

    let mut slug = String::new();
    let plans_dir = get_plans_directory();
    for _ in 0..MAX_SLUG_RETRIES {
        slug = crate::utils::words::generate_word_slug();
        if !crate::utils::fs_operations::get_fs_implementation()
            .exists_sync(&plans_dir.join(format!("{slug}.md")))
        {
            break;
        }
    }

    if let Ok(mut cache) = PLAN_SLUG_CACHE.write() {
        cache.insert(session_id, slug.clone());
    }
    slug
}

/// Maps to CC `utils/plans.ts` `setPlanSlug(...)`.
pub fn set_plan_slug(session_id: impl Into<String>, slug: impl Into<String>) {
    if let Ok(mut cache) = PLAN_SLUG_CACHE.write() {
        cache.insert(session_id.into(), slug.into());
    }
}

/// Maps to CC `utils/plans.ts` `clearPlanSlug(...)`.
pub fn clear_plan_slug(session_id: Option<&str>) {
    let session_id = session_id
        .map(str::to_string)
        .unwrap_or_else(crate::bootstrap::state::get_session_id);
    if let Ok(mut cache) = PLAN_SLUG_CACHE.write() {
        cache.remove(&session_id);
    }
}

/// Maps to CC `utils/plans.ts` `clearAllPlanSlugs()`.
pub fn clear_all_plan_slugs() {
    #[cfg(test)]
    let _guard = test_plan_state_lock();
    if let Ok(mut cache) = PLAN_SLUG_CACHE.write() {
        cache.clear();
    }
}

/// Maps to CC `utils/plans.ts` `getPlansDirectory()`.
pub fn get_plans_directory() -> PathBuf {
    let mut cache = PLANS_DIRECTORY_CACHE
        .write()
        .unwrap_or_else(|error| error.into_inner());
    cache
        .get_or_insert_with(|| {
            let settings = crate::utils::settings::get_initial_settings();
            get_plans_directory_with_settings(&settings)
        })
        .clone()
}

/// Maps to: CC `getPlansDirectory.cache.clear()` at worktree lifecycle boundaries.
pub fn clear_plans_directory_cache() {
    *PLANS_DIRECTORY_CACHE
        .write()
        .unwrap_or_else(|error| error.into_inner()) = None;
}

pub fn get_plans_directory_with_settings(settings: &SettingsJson) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if let Some(settings_dir) = settings
        .plans_directory
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        let resolved = normalize_path_lexically(if Path::new(settings_dir).is_absolute() {
            PathBuf::from(settings_dir)
        } else {
            cwd.join(settings_dir)
        });
        let cwd_normalized = normalize_path_lexically(cwd);
        if resolved == cwd_normalized || resolved.starts_with(&cwd_normalized) {
            return ensure_plans_directory(resolved);
        }
        crate::utils::log::log_error(crate::utils::log::LogError::new(format!(
            "plansDirectory must be within project root: {settings_dir}"
        )));
    }

    ensure_plans_directory(crate::utils::env_utils::get_claude_config_home_dir().join("plans"))
}

/// Maps to: CC `plans.ts:103-108` — `getPlansDirectory` itself owns the
/// `mkdirSync` (errors logged and swallowed), under the memoized resolver.
fn ensure_plans_directory(path: std::path::PathBuf) -> std::path::PathBuf {
    if let Err(error) = crate::utils::fs_operations::get_fs_implementation().mkdir_sync(&path, None)
    {
        crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
    }
    path
}

/// Maps to CC `utils/plans.ts` `getPlanFilePath(...)`.
pub fn get_plan_file_path(agent_id: Option<&str>) -> PathBuf {
    let plan_slug = get_plan_slug(Some(&crate::bootstrap::state::get_session_id()));
    let file_name = match agent_id.filter(|value| !value.is_empty()) {
        Some(agent_id) => format!("{plan_slug}-agent-{agent_id}.md"),
        None => format!("{plan_slug}.md"),
    };
    get_plans_directory().join(file_name)
}

/// Maps to CC `utils/plans.ts` `getPlan(agentId?)`.
pub fn get_plan(agent_id: Option<&str>) -> Option<String> {
    let file_path = get_plan_file_path(agent_id);
    match crate::utils::fs_operations::get_fs_implementation()
        .read_file_sync(
            &file_path,
            crate::utils::fs_operations::BufferEncoding::Utf8,
        )
        .map(|text| text.to_string_lossy())
    {
        Ok(content) => Some(content),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            crate::utils::log::log_error(crate::utils::log::LogError::new(err.to_string()));
            None
        }
    }
}

/// Maps to: CC `utils/plans.ts#getSlugFromLog:149-151`.
/// The borrowed slice carries `LogOption.messages`, preserving serialized slug
/// metadata without projecting the messages through the model-only types.
fn get_slug_from_log(messages: &[serde_json::Value]) -> Option<&str> {
    messages
        .iter()
        .filter_map(|message| message.get("slug").and_then(serde_json::Value::as_str))
        .find(|slug| !slug.is_empty())
}

/// Maps to: CC `utils/plans.ts#copyPlanForResume:164-231`.
/// Raw messages carry LogOption's persisted fields; eager preparation preserves
/// JavaScript execution through the first await, before a session switch.
pub fn copy_plan_for_resume(
    messages: &[serde_json::Value],
    target_session_id: Option<&str>,
) -> impl std::future::Future<Output = bool> + Send + 'static + use<> {
    let preparation = get_slug_from_log(messages).map(|slug| {
        let session_id = target_session_id
            .map(String::from)
            .unwrap_or_else(crate::bootstrap::state::get_session_id);
        set_plan_slug(session_id, slug);
        let plan_path = get_plans_directory().join(format!("{slug}.md"));
        // CC calls readFile before the first await: capture and start it now,
        // so a later setFsImplementation cannot change this request's backend.
        let read = crate::utils::fs_operations::get_fs_implementation().read_file(
            &plan_path,
            crate::utils::fs_operations::BufferEncoding::Utf8,
        );
        (plan_path, messages.to_vec(), read)
    });
    async move {
        let Some((plan_path, messages, read)) = preparation else {
            return false;
        };
        match read.await {
            Ok(_) => true,
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
                false
            }
            Err(_) => {
                if crate::utils::file_persistence::outputs_scanner::get_environment_kind().is_none()
                {
                    return false;
                }
                crate::utils::debug::log_for_debugging(&format!(
                    "Plan file missing during resume: {}. Attempting recovery.",
                    plan_path.display()
                ));
                let recovered = find_file_snapshot_entry(&messages, "plan")
                    .and_then(|entry| entry.get("content").and_then(serde_json::Value::as_str))
                    .filter(|content| !content.is_empty())
                    .map(|content| {
                        crate::utils::debug::log_for_debugging_with_level(
                            &format!(
                                "Plan recovered from file snapshot, {} chars",
                                content.encode_utf16().count()
                            ),
                            crate::utils::debug::DebugLogLevel::Info,
                        );
                        content
                    })
                    .or_else(|| {
                        recover_plan_from_messages(&messages).inspect(|content| {
                            crate::utils::debug::log_for_debugging_with_level(
                                &format!(
                                    "Plan recovered from message history, {} chars",
                                    content.encode_utf16().count()
                                ),
                                crate::utils::debug::DebugLogLevel::Info,
                            );
                        })
                    });
                if let Some(content) = recovered {
                    match tokio::fs::write(&plan_path, content).await {
                        Ok(()) => true,
                        Err(error) => {
                            crate::utils::log::log_error(crate::utils::log::LogError::new(
                                error.to_string(),
                            ));
                            false
                        }
                    }
                } else {
                    crate::utils::debug::log_for_debugging(
                        "Plan file recovery failed: no file snapshot or plan content found in message history",
                    );
                    false
                }
            }
        }
    }
}

/// Maps to: CC `utils/plans.ts#recoverPlanFromMessages:279-326`.
fn recover_plan_from_messages(messages: &[serde_json::Value]) -> Option<&str> {
    for message in messages.iter().rev() {
        let plan = match message.get("type").and_then(serde_json::Value::as_str) {
            Some("assistant") => message.pointer("/message/content")
                .and_then(serde_json::Value::as_array)
                .and_then(|content| content.iter().find_map(|block| {
                    (block["type"] == "tool_use" && block["name"] == crate::tools::exit_plan_mode_tool::constants::EXIT_PLAN_MODE_V2_TOOL_NAME)
                        .then(|| block.pointer("/input/plan").and_then(serde_json::Value::as_str))
                        .flatten().filter(|plan| !plan.is_empty())
                })),
            Some("user") => message.get("planContent").and_then(serde_json::Value::as_str),
            Some("attachment") if message.pointer("/attachment/type").and_then(serde_json::Value::as_str) == Some("plan_file_reference") =>
                message.pointer("/attachment/planContent").and_then(serde_json::Value::as_str),
            _ => None,
        };
        if let Some(plan) = plan.filter(|plan| !plan.is_empty()) {
            return Some(plan);
        }
    }
    None
}

/// Maps to: CC `utils/plans.ts#findFileSnapshotEntry:332-353`.
fn find_file_snapshot_entry<'a>(
    messages: &'a [serde_json::Value],
    key: &str,
) -> Option<&'a serde_json::Value> {
    for message in messages.iter().rev() {
        if message["type"] == "system"
            && message["subtype"] == "file_snapshot"
            && let Some(files) = message.get("snapshotFiles")
        {
            // Source returns from the newest snapshot even if its key is absent.
            return files
                .as_array()
                .and_then(|files| files.iter().find(|file| file["key"] == key));
        }
    }
    None
}

/// Maps to: CC `utils/plans.ts#persistFileSnapshotIfRemote:360-397`.
/// Snapshot construction is eager; transcript submission follows the async seam.
pub fn persist_file_snapshot_if_remote() -> impl std::future::Future<Output = ()> + Send + 'static {
    let message =
        if crate::utils::file_persistence::outputs_scanner::get_environment_kind().is_some() {
            get_plan(None).filter(|plan| !plan.is_empty()).map(|plan| serde_json::json!({
            "type": "system", "subtype": "file_snapshot", "content": "File snapshot",
            "level": "info", "isMeta": true,
            "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "uuid": uuid::Uuid::new_v4().to_string(),
            "snapshotFiles": [{"key": "plan", "path": get_plan_file_path(None), "content": plan}]
        }))
        } else {
            None
        };
    async move {
        if let Some(message) = message {
            let session_id = crate::bootstrap::state::get_session_id();
            let cwd = std::env::current_dir()
                .unwrap_or_else(|_| crate::bootstrap::state::get_original_cwd())
                .to_string_lossy()
                .into_owned();
            let stamp = crate::utils::session_storage::SessionStamp {
                session_id: session_id.clone(),
                cwd: cwd.clone(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                git_branch: Some(crate::utils::git::get_branch()).filter(|value| !value.is_empty()),
                user_type: crate::utils::build_profile::build_audience()
                    .as_str()
                    .to_string(),
                entrypoint: "cli".to_string(),
                slug: Some(get_plan_slug(Some(&session_id))),
                team_name: None,
                agent_name: None,
            };
            if let Err(error) = crate::utils::session_storage::record_transcript(
                &cwd,
                &session_id,
                &[message],
                None,
                &stamp,
            ) {
                crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
            }
        }
    }
}

/// Maps to: CC `utils/plans.ts#copyPlanForFork:239-264`.
/// `messages` borrows the only `LogOption` field consumed by this operation.
/// The caller owns the source's fire-and-forget scheduling; a missing plan
/// returns false after assigning the target slug, without transcript recovery.
/// JavaScript runs through `getPlanSlug` before its first await. Returning an
/// owned future from an ordinary function preserves that eager cache write;
/// an `async fn` would defer it until polling and race transcript slug writes.
pub fn copy_plan_for_fork(
    messages: &[serde_json::Value],
    target_session_id: &str,
) -> impl std::future::Future<Output = bool> + Send + 'static + use<> {
    let paths = get_slug_from_log(messages).map(|original_slug| {
        let plans_dir = get_plans_directory();
        let original_plan_path = plans_dir.join(format!("{original_slug}.md"));
        let new_slug = get_plan_slug(Some(target_session_id));
        let new_plan_path = plans_dir.join(format!("{new_slug}.md"));
        (original_plan_path, new_plan_path)
    });
    async move {
        let Some((original_plan_path, new_plan_path)) = paths else {
            return false;
        };
        match tokio::fs::copy(&original_plan_path, &new_plan_path).await {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
                false
            }
        }
    }
}

fn normalize_path_lexically(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::Prefix(_)
            | std::path::Component::RootDir
            | std::path::Component::Normal(_) => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    struct PlanFixtureDirectory(std::path::PathBuf);
    impl PlanFixtureDirectory {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("cometix-branch-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for PlanFixtureDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, PinnedProjectDir, TEST_ENV_LOCK};

    #[test]
    fn get_slug_from_log_matches_official_first_truthy_slug() {
        // CC utils/plans.ts:149-151: find, not latest/trim/type filtering.
        let messages = serde_json::json!([
            {"type": "user"},
            {"type": "assistant", "slug": ""},
            {"type": "system", "slug": " "},
            {"type": "user", "slug": "later-slug"}
        ]);
        assert_eq!(get_slug_from_log(messages.as_array().unwrap()), Some(" "));
        assert_eq!(get_slug_from_log(&[]), None);
        assert_eq!(get_slug_from_log(&[serde_json::json!({"slug": ""})]), None);
    }

    #[test]
    fn copy_plan_for_fork_matches_official_eager_slug_and_independent_file() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _plan_lock = test_plan_state_lock();
        let temp = PlanFixtureDirectory::new();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path());
        let _project = PinnedProjectDir::at(temp.path());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let source_id = uuid::Uuid::new_v4().to_string();
        let target_id = uuid::Uuid::new_v4().to_string();
        let source_slug = format!("source-{source_id}");
        set_plan_slug(&source_id, &source_slug);
        let plans_dir = get_plans_directory();
        let source_path = plans_dir.join(format!("{source_slug}.md"));
        std::fs::write(&source_path, b"# Plan\n\0original\n").unwrap();
        let messages = vec![serde_json::json!({"slug": source_slug})];

        // CC :249-255 assigns the new slug before the first await. The future
        // owns its paths and may outlive the source log and ID borrow.
        let copying = copy_plan_for_fork(&messages, &target_id);
        let target_slug = PLAN_SLUG_CACHE
            .read()
            .unwrap()
            .get(&target_id)
            .cloned()
            .unwrap();
        assert_ne!(target_slug, source_slug);
        assert_eq!(target_slug.split('-').count(), 3);
        let target_path = plans_dir.join(format!("{target_slug}.md"));
        drop(messages);
        assert!(runtime.block_on(copying));
        assert_eq!(
            std::fs::read(&target_path).unwrap(),
            std::fs::read(&source_path).unwrap()
        );
        std::fs::write(&target_path, b"fork edit").unwrap();
        assert_eq!(
            std::fs::read(&source_path).unwrap(),
            b"# Plan\n\0original\n"
        );
        assert_eq!(get_plan_slug(Some(&source_id)), source_slug);
        clear_plan_slug(Some(&source_id));
        clear_plan_slug(Some(&target_id));
    }

    #[test]
    fn copy_plan_for_fork_matches_official_missing_slug_and_copy_failures() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _plan_lock = test_plan_state_lock();
        let temp = PlanFixtureDirectory::new();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path());
        let _project = PinnedProjectDir::at(temp.path());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let target_id = uuid::Uuid::new_v4().to_string();

        // CC :243-246 exits before resolving plansDir or generating a slug.
        assert!(!runtime.block_on(copy_plan_for_fork(&[], &target_id)));
        assert!(!PLAN_SLUG_CACHE.read().unwrap().contains_key(&target_id));
        assert!(!temp.path().join("plans").exists());

        // CC :252 runs even when copyFile later fails with ENOENT. There is
        // no fallback to planContent or rollback of the newly cached slug.
        let missing = vec![serde_json::json!({
            "type": "user", "slug": "missing-source", "planContent": "must not recover"
        })];
        assert!(!runtime.block_on(copy_plan_for_fork(&missing, &target_id)));
        let target_slug = get_plan_slug(Some(&target_id));
        let plans_dir = get_plans_directory();
        assert!(!plans_dir.join(format!("{target_slug}.md")).exists());

        // A directory source is a non-ENOENT copy error (:261-262), also false.
        std::fs::create_dir(plans_dir.join("directory-source.md")).unwrap();
        assert!(!runtime.block_on(copy_plan_for_fork(
            &[serde_json::json!({"slug": "directory-source"})],
            &target_id
        )));
        assert_eq!(get_plan_slug(Some(&target_id)), target_slug);
        clear_plan_slug(Some(&target_id));
    }

    #[test]
    fn get_plan_slug_matches_official_empty_cache_regeneration() {
        let _plan_lock = test_plan_state_lock();
        let session_id = uuid::Uuid::new_v4().to_string();
        set_plan_slug(&session_id, "");
        let slug = get_plan_slug(Some(&session_id));
        assert!(!slug.is_empty());
        assert_eq!(slug.split('-').count(), 3);
        assert_eq!(get_plan_slug(Some(&session_id)), slug);
        clear_plan_slug(Some(&session_id));
    }

    #[test]
    fn plan_file_path_uses_session_slug_and_optional_agent_suffix() {
        let _guard = test_plan_state_lock();
        let session_id = format!("plan-test-{}", uuid::Uuid::new_v4());
        set_plan_slug(&session_id, "calm-river");
        let previous_session = crate::bootstrap::state::get_session_id();
        crate::bootstrap::state::set_session_id(&session_id);

        let main = get_plan_file_path(None);
        let agent = get_plan_file_path(Some("agent-1"));

        assert!(main.ends_with(Path::new("calm-river.md")));
        assert!(agent.ends_with(Path::new("calm-river-agent-agent-1.md")));
        crate::bootstrap::state::set_session_id(previous_session);
        clear_plan_slug(Some(&session_id));
    }

    #[test]
    fn plans_directory_setting_must_stay_within_project() {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut settings = SettingsJson {
            plans_directory: Some(".claude/plans".to_string()),
            ..SettingsJson::default()
        };
        assert_eq!(
            get_plans_directory_with_settings(&settings),
            normalize_path_lexically(cwd.join(".claude/plans"))
        );

        settings.plans_directory = Some("../outside".to_string());
        assert_eq!(
            get_plans_directory_with_settings(&settings),
            crate::utils::env_utils::get_claude_config_home_dir().join("plans")
        );
    }
    #[test]
    fn copy_plan_for_resume_matches_official_local_remote_and_eager_slug() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _plan_lock = test_plan_state_lock();
        let temp = PlanFixtureDirectory::new();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path());
        let _project = PinnedProjectDir::at(temp.path());
        let _local = EnvVarGuard::set("CLAUDE_CODE_ENVIRONMENT_KIND", "");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let target = uuid::Uuid::new_v4().to_string();
        let messages = vec![
            serde_json::json!({"type":"user","slug":"resumed-source","planContent":"from messages"}),
        ];
        let copying = copy_plan_for_resume(&messages, Some(&target));
        assert_eq!(
            PLAN_SLUG_CACHE
                .read()
                .unwrap()
                .get(&target)
                .map(String::as_str),
            Some("resumed-source")
        );
        let path = get_plans_directory().join("resumed-source.md");
        assert!(!runtime.block_on(copying));
        assert!(!path.exists());
        std::fs::write(&path, b"").unwrap();
        assert!(runtime.block_on(copy_plan_for_resume(&messages, Some(&target))));
        assert_eq!(std::fs::read(&path).unwrap(), b"");
        std::fs::remove_file(&path).unwrap();
        {
            let _remote = EnvVarGuard::set("CLAUDE_CODE_ENVIRONMENT_KIND", "byoc");
            let mut snapshot_messages = messages.clone();
            snapshot_messages.push(serde_json::json!({"type":"system","subtype":"file_snapshot","snapshotFiles":[{"key":"plan","path":"old/location","content":"snapshot wins"}]}));
            assert!(runtime.block_on(copy_plan_for_resume(&snapshot_messages, Some(&target))));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "snapshot wins");
            std::fs::remove_file(&path).unwrap();
            snapshot_messages.push(
                serde_json::json!({"type":"system","subtype":"file_snapshot","snapshotFiles":[]}),
            );
            assert!(runtime.block_on(copy_plan_for_resume(&snapshot_messages, Some(&target))));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "from messages");
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir(&path).unwrap();
            // Non-ENOENT read error: no fallback or write attempt.
            assert!(!runtime.block_on(copy_plan_for_resume(&snapshot_messages, Some(&target))));
        }
        let absent = uuid::Uuid::new_v4().to_string();
        assert!(!runtime.block_on(copy_plan_for_resume(&[], Some(&absent))));
        assert!(!PLAN_SLUG_CACHE.read().unwrap().contains_key(&absent));
        clear_plan_slug(Some(&target));
        clear_plans_directory_cache();
    }

    #[test]
    fn plans_directory_matches_official_memo_and_explicit_invalidation() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _plan_lock = test_plan_state_lock();
        let temp = PlanFixtureDirectory::new();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path());
        let _project = PinnedProjectDir::at(temp.path());
        let path = get_plans_directory();
        std::fs::remove_dir(&path).unwrap();
        assert_eq!(get_plans_directory(), path);
        assert!(!path.exists(), "memoized resolver must not repeat mkdir");
        clear_plans_directory_cache();
        assert_eq!(get_plans_directory(), path);
        assert!(path.is_dir());
        let settings = SettingsJson {
            plans_directory: Some(" ".to_string()),
            ..SettingsJson::default()
        };
        assert_eq!(
            get_plans_directory_with_settings(&settings),
            std::env::current_dir().unwrap().join(" ")
        );
        clear_plans_directory_cache();
    }

    #[test]
    fn plan_recovery_matches_actual_original_bun_message_oracle() {
        // AST-extracted source functions, Bun oracle 2026-09-12.
        let cases: serde_json::Value = serde_json::from_str(
            r###"[
  {
    "messages": [],
    "slug": null,
    "recovered": null,
    "snapshot": null
  },
  {
    "messages": [
      {
        "type": "user",
        "planContent": "older"
      },
      {
        "type": "attachment",
        "attachment": {
          "type": "plan_file_reference",
          "planContent": "attached"
        }
      }
    ],
    "slug": null,
    "recovered": "attached",
    "snapshot": null
  },
  {
    "messages": [
      {
        "type": "user",
        "planContent": "older"
      },
      {
        "type": "assistant",
        "message": {
          "content": [
            {
              "type": "tool_use",
              "name": "ExitPlanMode",
              "input": {
                "plan": "first"
              }
            },
            {
              "type": "tool_use",
              "name": "ExitPlanMode",
              "input": {
                "plan": "second"
              }
            }
          ]
        }
      }
    ],
    "slug": null,
    "recovered": "first",
    "snapshot": null
  },
  {
    "messages": [
      {
        "type": "user",
        "planContent": "older"
      },
      {
        "type": "assistant",
        "message": {
          "content": [
            {
              "type": "tool_use",
              "name": "ExitPlanMode",
              "input": {
                "plan": ""
              }
            },
            {
              "type": "tool_use",
              "name": "ExitPlanMode",
              "input": {
                "plan": "later"
              }
            }
          ]
        }
      }
    ],
    "slug": null,
    "recovered": "later",
    "snapshot": null
  },
  {
    "messages": [
      {
        "type": "system",
        "subtype": "file_snapshot",
        "snapshotFiles": [
          {
            "key": "plan",
            "path": "p",
            "content": "old"
          }
        ]
      },
      {
        "type": "system",
        "subtype": "file_snapshot",
        "snapshotFiles": [
          {
            "key": "todos",
            "path": "t",
            "content": "todo"
          }
        ]
      }
    ],
    "slug": null,
    "recovered": null,
    "snapshot": null
  },
  {
    "messages": [
      {
        "type": "system",
        "subtype": "file_snapshot",
        "snapshotFiles": [
          {
            "key": "plan",
            "path": "p",
            "content": "old"
          }
        ]
      },
      {
        "type": "system",
        "subtype": "file_snapshot",
        "snapshotFiles": []
      }
    ],
    "slug": null,
    "recovered": null,
    "snapshot": null
  },
  {
    "messages": [
      {
        "type": "user",
        "slug": ""
      },
      {
        "type": "user",
        "slug": " "
      },
      {
        "type": "user",
        "slug": "later"
      }
    ],
    "slug": " ",
    "recovered": null,
    "snapshot": null
  }
]
"###,
        )
        .unwrap();
        for case in cases.as_array().unwrap() {
            let messages = case["messages"].as_array().unwrap();
            assert_eq!(get_slug_from_log(messages), case["slug"].as_str());
            assert_eq!(
                recover_plan_from_messages(messages),
                case["recovered"].as_str()
            );
            assert_eq!(
                find_file_snapshot_entry(messages, "plan"),
                if case["snapshot"].is_null() {
                    None
                } else {
                    Some(&case["snapshot"])
                }
            );
        }
    }
    #[test]
    fn remote_file_snapshot_matches_official_wire_and_incremental_persistence() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _plan_lock = test_plan_state_lock();
        // Session storage's test profile defaults to read-only. Enable the
        // actual writer so this exercises CC plans.ts:392-393 recordTranscript.
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let temp = PlanFixtureDirectory::new();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path());
        let _project = PinnedProjectDir::at(temp.path());
        let _remote = EnvVarGuard::set("CLAUDE_CODE_ENVIRONMENT_KIND", "anthropic_cloud");
        let old_id = crate::bootstrap::state::get_session_id();
        let old_dir = crate::bootstrap::state::get_session_project_dir();
        let id = uuid::Uuid::new_v4().to_string();
        let transcript_dir = temp.path().join("transcripts");
        std::fs::create_dir_all(&transcript_dir).unwrap();
        crate::bootstrap::state::switch_session(&id, Some(transcript_dir.clone()));
        crate::utils::session_storage::clear_session_metadata();
        crate::utils::session_storage::clear_session_messages_cache();
        set_plan_slug(&id, "snapshot-plan");
        let path = get_plan_file_path(None);
        std::fs::write(&path, "# original\n").unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // Source snapshots before its first await (dynamic sessionStorage import).
        let snapshot = persist_file_snapshot_if_remote();
        std::fs::write(&path, "# later\n").unwrap();
        runtime.block_on(snapshot);
        let stamp = crate::utils::session_storage::SessionStamp {
            session_id: id.clone(),
            cwd: temp.path().to_string_lossy().into_owned(),
            version: "test".into(),
            git_branch: None,
            user_type: "external".into(),
            entrypoint: "cli".into(),
            slug: Some("snapshot-plan".into()),
            team_name: None,
            agent_name: None,
        };
        crate::utils::session_storage::record_transcript(
            &stamp.cwd,
            &id,
            &[serde_json::json!({
                "type":"user","uuid":"seed-snapshot-user","message":{"role":"user","content":"seed"}
            })],
            None,
            &stamp,
        )
        .unwrap();
        runtime
            .block_on(crate::utils::session_storage::flush_session_storage())
            .unwrap();
        let rows = crate::utils::json::parse_jsonl(
            &std::fs::read(transcript_dir.join(format!("{id}.jsonl"))).unwrap(),
        );
        let snapshots = rows
            .iter()
            .filter(|row| row["subtype"] == "file_snapshot")
            .collect::<Vec<_>>();
        assert_eq!(snapshots.len(), 1);
        let row = snapshots[0];
        assert_eq!(row["type"], "system");
        assert_eq!(row["content"], "File snapshot");
        assert_eq!(row["level"], "info");
        assert_eq!(row["isMeta"], true);
        assert_eq!(
            row["snapshotFiles"],
            serde_json::json!([{"key":"plan","path":path,"content":"# original\n"}])
        );
        assert!(uuid::Uuid::parse_str(row["uuid"].as_str().unwrap()).is_ok());
        assert!(chrono::DateTime::parse_from_rfc3339(row["timestamp"].as_str().unwrap()).is_ok());
        clear_plan_slug(Some(&id));
        clear_plans_directory_cache();
        crate::utils::session_storage::clear_session_metadata();
        crate::utils::session_storage::clear_session_messages_cache();
        crate::bootstrap::state::switch_session(&old_id, old_dir);
    }
}
