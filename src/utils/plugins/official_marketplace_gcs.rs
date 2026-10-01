//! Maps to: CC `utils/plugins/officialMarketplaceGcs.ts`.
//! Reqwest represents Axios's transport; its platform proxy/TLS/error wording
//! remains a native boundary. Source mirror/sentinel/staging/telemetry policy
//! is retained. See dxt::zip for the bounded archive transport representation.
use std::path::{Path, PathBuf};
/// Maps to: CC `utils/plugins/officialMarketplaceGcs.ts:28-29#GCS_BASE`.
const GCS_BASE: &str =
    "https://downloads.claude.ai/claude-code-releases/plugins/claude-plugins-official";
/// Maps to: CC `utils/plugins/officialMarketplaceGcs.ts:34-34#ARC_PREFIX`.
const ARC_PREFIX: &str = "marketplaces/claude-plugins-official/";
// Native Node path.join/resolve representation, with no domain decisions.
macro_rules! gcs_path {
    ($base:expr,$relative:expr) => {{
        let joined = format!(
            "{}/{}",
            AsRef::<Path>::as_ref(&$base).display(),
            AsRef::<Path>::as_ref(&$relative).display()
        );
        let mut path = PathBuf::new();
        for part in Path::new(&joined).components() {
            match part {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    if path.file_name().is_some_and(|s| s != "..") {
                        path.pop();
                    } else if !path.has_root() {
                        path.push("..");
                    }
                }
                p => path.push(p.as_os_str()),
            }
        }
        path
    }};
}
/// Maps to: CC `utils/plugins/officialMarketplaceGcs.ts:47-170#fetchOfficialMarketplaceFromGcs`.
pub async fn fetch_official_marketplace_from_gcs(
    install_location: &Path,
    marketplaces_cache_dir: &Path,
) -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let cache_dir = if marketplaces_cache_dir.is_absolute() {
        gcs_path!(marketplaces_cache_dir, "")
    } else {
        gcs_path!(&cwd, marketplaces_cache_dir)
    };
    let location = if install_location.is_absolute() {
        gcs_path!(install_location, "")
    } else {
        gcs_path!(&cwd, install_location)
    };
    if !location.starts_with(&cache_dir) {
        crate::utils::debug::log_for_debugging_with_level(
            &format!(
                "fetchOfficialMarketplaceFromGcs: refusing path outside cache dir: {}",
                install_location.display()
            ),
            crate::utils::debug::DebugLogLevel::Error,
        );
        return None;
    }
    crate::bootstrap::state::wait_for_scroll_idle().await;
    let start = std::time::Instant::now();
    let mut outcome = "failed";
    let mut sha = None;
    let mut bytes = None;
    let mut err_kind = None;
    let fetched:anyhow::Result<String>=async {
        // CC `axios.get` (`officialMarketplaceGcs.ts:81-84,107-110`), through
        // the global interceptor.
        let client=crate::utils::proxy::create_axios_instance()?.build()?;
        let latest=client.get(format!("{GCS_BASE}/latest")).timeout(std::time::Duration::from_secs(10)).send().await?.error_for_status()?.text().await?;
        let latest=latest.trim_matches(|c:char|matches!(c,'\u{9}'..='\u{d}'|'\u{20}'|'\u{a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')).to_owned();sha=Some(latest.clone());if latest.is_empty(){anyhow::bail!("latest pointer returned empty body");}
        let sentinel=gcs_path!(install_location,".gcs-sha");let current=tokio::fs::read(&sentinel).await.ok().map(|b|String::from_utf8_lossy(&b).trim_matches(|c:char|matches!(c,'\u{9}'..='\u{d}'|'\u{20}'|'\u{a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')).to_owned());
        if current.as_deref()==Some(&latest){outcome="noop";return Ok(latest);}
        let zip=client.get(format!("{GCS_BASE}/{latest}.zip")).timeout(std::time::Duration::from_secs(60)).send().await?.error_for_status()?.bytes().await?;bytes=Some(zip.len());
        let files=crate::utils::dxt::zip::unzip_file(&zip).await?;let modes=crate::utils::dxt::zip::parse_zip_modes(&zip);
        let staging=PathBuf::from(format!("{}.staging",install_location.display()));crate::utils::fs_operations::native::rm(&staging, crate::utils::fs_operations::RmOptions { recursive: true, force: true }).await?;tokio::fs::create_dir_all(&staging).await?;
        for(arc_path,data)in files{
            let Some(relative)=arc_path.strip_prefix(ARC_PREFIX)else{continue};if relative.is_empty()||relative.ends_with('/'){continue;}
            let dest=gcs_path!(&staging,relative);tokio::fs::create_dir_all(gcs_path!(&dest,"..")).await?;tokio::fs::write(&dest,data).await?;
            if let Some(&mode)=modes.get(&arc_path).filter(|&&m|m&0o111!=0){
                #[cfg(unix)]{use std::os::unix::fs::PermissionsExt;let _=tokio::fs::set_permissions(&dest,std::fs::Permissions::from_mode(mode&0o777)).await;}
                #[cfg(not(unix))]{let _=mode;}
            }
        }
        tokio::fs::write(gcs_path!(&staging,".gcs-sha"),&latest).await?;crate::utils::fs_operations::native::rm(install_location, crate::utils::fs_operations::RmOptions { recursive: true, force: true }).await?;tokio::fs::rename(&staging,install_location).await?;outcome="updated";Ok(latest)
    }.await;
    let result = match fetched {
        Ok(sha) => Some(sha),
        Err(error) => {
            err_kind = Some(classify_gcs_error(&error));
            crate::utils::debug::log_for_debugging_with_level(
                &format!("Official marketplace GCS fetch failed: {error}"),
                crate::utils::debug::DebugLogLevel::Warn,
            );
            None
        }
    };
    let mut metadata = serde_json::json!({"source":"marketplace_gcs","host":"downloads.claude.ai","is_official":true,"outcome":outcome,"duration_ms":(start.elapsed().as_secs_f64()*1000.0).round()});
    if let Some(bytes) = bytes {
        metadata["bytes"] = bytes.into();
    }
    if let Some(sha) = sha.filter(|s| !s.is_empty()) {
        metadata["sha"] = sha.into();
    }
    if let Some(kind) = err_kind {
        metadata["error_kind"] = kind.into();
    }
    crate::services::analytics::log_event("tengu_plugin_remote_fetch", metadata);
    result
}
/// Maps to: CC `utils/plugins/officialMarketplaceGcs.ts:174-185#KNOWN_FS_CODES`.
const KNOWN_FS_CODES: &[&str] = &[
    "ENOSPC",
    "EACCES",
    "EPERM",
    "EXDEV",
    "EBUSY",
    "ENOENT",
    "ENOTDIR",
    "EROFS",
    "EMFILE",
    "ENAMETOOLONG",
];
/// Maps to: CC `utils/plugins/officialMarketplaceGcs.ts:196-216#classifyGcsError`.
pub fn classify_gcs_error(error: &anyhow::Error) -> String {
    if let Some(error) = error.downcast_ref::<reqwest::Error>() {
        if error.is_timeout() {
            return "timeout".into();
        }
        if let Some(status) = error.status() {
            return format!("http_{}", status.as_u16());
        }
        return "network".into();
    }
    if let Some(code) = crate::utils::errors::get_errno_code(error) {
        if code.starts_with('E') && code[1..].bytes().all(|b| b.is_ascii_uppercase()) {
            return if KNOWN_FS_CODES.contains(&code) {
                format!("fs_{code}")
            } else {
                "fs_other".into()
            };
        }
    }
    if error.is::<crate::utils::dxt::zip::ZipParseError>() {
        return "zip_parse".into();
    }
    let msg = error.to_string();
    if regress::Regex::with_flags("unzip|invalid zip|central directory", "i")
        .unwrap()
        .find(&msg)
        .is_some()
    {
        return "zip_parse".into();
    }
    if msg.contains("empty body") {
        return "empty_latest".into();
    }
    "other".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gcs_error_buckets_match_original() {
        assert_eq!(
            classify_gcs_error(&std::io::Error::from_raw_os_error(libc::EACCES).into()),
            "fs_EACCES"
        );
        assert_eq!(
            classify_gcs_error(&crate::utils::dxt::zip::ZipParseError("bad".into()).into()),
            "zip_parse"
        );
        assert_eq!(
            classify_gcs_error(&anyhow::anyhow!("INVALID ZIP")),
            "zip_parse"
        );
        assert_eq!(
            classify_gcs_error(&anyhow::anyhow!("latest pointer returned empty body")),
            "empty_latest"
        );
        assert_eq!(classify_gcs_error(&anyhow::anyhow!("other")), "other");
    }
    #[tokio::test]
    async fn outside_cache_is_rejected_before_network() {
        assert!(
            fetch_official_marketplace_from_gcs(Path::new("/outside"), Path::new("/cache"))
                .await
                .is_none()
        );
    }
}
