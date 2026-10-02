//! Maps to: CC `utils/plugins/zipCache.ts`.
//! Native archive encoding uses the existing flate2 DEFLATE engine in place
//! of fflate; compressed byte streams/timestamps are not byte-identical, but
//! file order, content and Unix mode attributes retain the source contract.
use crate::utils::process_env::JsTruthy;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
// Node path.join built-in boundary (no business policy; normalize all parts).
macro_rules! zip_path {
    ($base:expr,$part:expr) => {{
        let s = format!(
            "{}/{}",
            AsRef::<Path>::as_ref(&$base).display(),
            AsRef::<Path>::as_ref(&$part).display()
        );
        let mut out = PathBuf::new();
        for c in Path::new(&s).components() {
            match c {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    if out.file_name().is_some_and(|n| n != "..") {
                        out.pop();
                    } else if !out.has_root() {
                        out.push("..");
                    }
                }
                c => out.push(c.as_os_str()),
            }
        }
        out
    }};
}
/// Maps to: CC `utils/plugins/zipCache.ts:55-57#isPluginZipCacheEnabled`.
pub fn is_plugin_zip_cache_enabled() -> bool {
    crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_PLUGIN_USE_ZIP_CACHE").as_deref(),
    )
}
/// Maps to: CC `utils/plugins/zipCache.ts:64-70#getPluginZipCachePath`.
pub fn get_plugin_zip_cache_path() -> Option<PathBuf> {
    if !is_plugin_zip_cache_enabled() {
        return None;
    }
    crate::utils::process_env::var("CLAUDE_CODE_PLUGIN_CACHE_DIR")
        .truthy()
        .map(|s| PathBuf::from(crate::utils::permissions::path_validation::expand_tilde(&s)))
}
/// Maps to: CC `utils/plugins/zipCache.ts:75-81#getZipCacheKnownMarketplacesPath`.
pub fn get_zip_cache_known_marketplaces_path() -> anyhow::Result<PathBuf> {
    let cache = get_plugin_zip_cache_path()
        .ok_or_else(|| anyhow::anyhow!("Plugin zip cache is not enabled"))?;
    Ok(zip_path!(cache, "known_marketplaces.json"))
}
/// Maps to: CC `utils/plugins/zipCache.ts:86-92#getZipCacheInstalledPluginsPath`.
pub fn get_zip_cache_installed_plugins_path() -> anyhow::Result<PathBuf> {
    let cache = get_plugin_zip_cache_path()
        .ok_or_else(|| anyhow::anyhow!("Plugin zip cache is not enabled"))?;
    Ok(zip_path!(cache, "installed_plugins.json"))
}
/// Maps to: CC `utils/plugins/zipCache.ts:97-103#getZipCacheMarketplacesDir`.
pub fn get_zip_cache_marketplaces_dir() -> anyhow::Result<PathBuf> {
    let cache = get_plugin_zip_cache_path()
        .ok_or_else(|| anyhow::anyhow!("Plugin zip cache is not enabled"))?;
    Ok(zip_path!(cache, "marketplaces"))
}
/// Maps to: CC `utils/plugins/zipCache.ts:108-114#getZipCachePluginsDir`.
pub fn get_zip_cache_plugins_dir() -> anyhow::Result<PathBuf> {
    let cache = get_plugin_zip_cache_path()
        .ok_or_else(|| anyhow::anyhow!("Plugin zip cache is not enabled"))?;
    Ok(zip_path!(cache, "plugins"))
}
/// Maps to: CC `utils/plugins/zipCache.ts:120-121` session path/Promise state.
type SessionCachePromise =
    futures::future::Shared<futures::future::BoxFuture<'static, Result<PathBuf, Arc<String>>>>;
static SESSION_PLUGIN_CACHE_PATH: LazyLock<Mutex<Option<PathBuf>>> =
    LazyLock::new(Default::default);
static SESSION_PLUGIN_CACHE_PROMISE: LazyLock<Mutex<Option<SessionCachePromise>>> =
    LazyLock::new(Default::default);
/// Maps to: CC `utils/plugins/zipCache.ts:125-140#getSessionPluginCachePath`.
/// PORTING A6/A7: memo Promise creation is eager and survives dropped callers.
pub fn get_session_plugin_cache_path()
-> impl std::future::Future<Output = anyhow::Result<PathBuf>> + Send + 'static {
    use futures::FutureExt;
    let cached = SESSION_PLUGIN_CACHE_PATH.lock().unwrap().clone();
    let future = if let Some(path) = cached {
        futures::future::ready(Ok(path)).boxed().shared()
    } else {
        let mut promise = SESSION_PLUGIN_CACHE_PROMISE.lock().unwrap();
        promise
            .get_or_insert_with(|| {
                let future = async {
                    let result: anyhow::Result<PathBuf> = async {
                        let mut random = [0u8; 8];
                        getrandom::fill(&mut random).map_err(|e| anyhow::anyhow!(e.to_string()))?;
                        let suffix = random
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect::<String>();
                        let dir = zip_path!(
                            crate::utils::node_os::tmpdir(),
                            format!("claude-plugin-session-{suffix}")
                        );
                        crate::utils::fs_operations::get_fs_implementation()
                            .mkdir(&(&dir), None)
                            .await?;
                        *SESSION_PLUGIN_CACHE_PATH.lock().unwrap() = Some(dir.clone());
                        crate::utils::debug::log_for_debugging(&format!(
                            "Created session plugin cache at {}",
                            dir.display()
                        ));
                        Ok(dir)
                    }
                    .await;
                    result.map_err(|e| Arc::new(e.to_string()))
                }
                .boxed()
                .shared();
                let worker = future.clone();
                crate::utils::process_runtime::runtime_handle_for_detached_work()
                    .expect("session plugin cache requires process lifetime runtime")
                    .spawn(async move {
                        let _ = worker.await;
                    });
                future
            })
            .clone()
    };
    async move {
        future
            .await
            .map_err(|e| anyhow::Error::msg(e.as_str().to_owned()))
    }
}
/// Maps to: CC `utils/plugins/zipCache.ts:146-161#cleanupSessionPluginCache`.
pub async fn cleanup_session_plugin_cache() {
    let Some(path) = SESSION_PLUGIN_CACHE_PATH.lock().unwrap().clone() else {
        return;
    };
    match crate::utils::fs_operations::native::rm(
        &path,
        crate::utils::fs_operations::RmOptions {
            recursive: true,
            force: true,
        },
    )
    .await
    {
        Ok(()) => crate::utils::debug::log_for_debugging(&format!(
            "Cleaned up session plugin cache at {}",
            path.display()
        )),
        Err(error) => crate::utils::debug::log_for_debugging(&format!(
            "Failed to clean up session plugin cache: {error}"
        )),
    }
    *SESSION_PLUGIN_CACHE_PATH.lock().unwrap() = None;
    *SESSION_PLUGIN_CACHE_PROMISE.lock().unwrap() = None;
}
/// Maps to: CC `utils/plugins/zipCache.ts:166-169#resetSessionPluginCache`.
pub fn reset_session_plugin_cache() {
    *SESSION_PLUGIN_CACHE_PATH.lock().unwrap() = None;
    *SESSION_PLUGIN_CACHE_PROMISE.lock().unwrap() = None;
}
/// Maps to: CC `utils/plugins/zipCache.ts:175-201#atomicWriteToZipCache`.
/// UTF-8 text and Uint8Array share the source writeFile byte contract.
pub async fn atomic_write_to_zip_cache(target: &Path, data: &[u8]) -> anyhow::Result<()> {
    let dir = zip_path!(target, "..");
    crate::utils::fs_operations::get_fs_implementation()
        .mkdir(&(&dir), None)
        .await?;
    let mut random = [0u8; 4];
    getrandom::fill(&mut random).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let suffix = random
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let name = format!(
        ".{}.tmp.{suffix}",
        target.file_name().unwrap_or_default().to_string_lossy()
    );
    let temp = zip_path!(&dir, name);
    let result: anyhow::Result<()> = async {
        tokio::fs::write(&temp, data).await?;
        tokio::fs::rename(&temp, target).await?;
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = crate::utils::fs_operations::native::rm(
            &temp,
            crate::utils::fs_operations::RmOptions {
                recursive: false,
                force: true,
            },
        )
        .await;
    }
    result
}
/// Maps to: CC `utils/plugins/zipCache.ts:205-205#ZipEntry`.
type ZipEntry = (Vec<u8>, u32);
/// Maps to: CC `utils/plugins/zipCache.ts:216-229#createZipFromDirectory`.
pub async fn create_zip_from_directory(source_dir: &Path) -> anyhow::Result<Vec<u8>> {
    use chrono::{Datelike, Timelike};
    use std::io::Write;
    let mut files = indexmap::IndexMap::new();
    let mut visited = std::collections::HashSet::new();
    collect_files_for_zip(source_dir, "", &mut files, &mut visited).await;
    // Native fflate.zipSync representation: raw DEFLATE level 6 and standard
    // local/central/EOCD records. ZIP's CRC-32 is a format primitive, not policy.
    let mut zip = Vec::new();
    let mut central = Vec::new();
    let now = chrono::Local::now();
    let date = (((now.year() - 1980) as u16) << 9) | ((now.month() as u16) << 5) | now.day() as u16;
    let time =
        ((now.hour() as u16) << 11) | ((now.minute() as u16) << 5) | (now.second() as u16 / 2);
    for (name, (data, attrs)) in crate::utils::process_env::ecmascript_object_entries(files.iter())
    {
        let mut crc = 0xffff_ffffu32;
        for &byte in data {
            crc ^= byte as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        crc = !crc;
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
        encoder.write_all(data)?;
        let compressed = encoder.finish()?;
        let offset = zip.len() as u32;
        let name = name.as_bytes();
        if name.len() > 65535 {
            return Err(crate::utils::dxt::zip::ZipParseError("filename too long".into()).into());
        }
        let flags = if name.is_ascii() { 0u16 } else { 0x800 };
        zip.extend(0x04034b50u32.to_le_bytes());
        zip.extend(20u16.to_le_bytes());
        zip.extend(flags.to_le_bytes());
        zip.extend(8u16.to_le_bytes());
        zip.extend(time.to_le_bytes());
        zip.extend(date.to_le_bytes());
        zip.extend(crc.to_le_bytes());
        zip.extend((compressed.len() as u32).to_le_bytes());
        zip.extend((data.len() as u32).to_le_bytes());
        zip.extend((name.len() as u16).to_le_bytes());
        zip.extend(0u16.to_le_bytes());
        zip.extend(name);
        zip.extend(&compressed);
        central.extend(0x02014b50u32.to_le_bytes());
        central.extend(((3u16 << 8) | 20).to_le_bytes());
        central.extend(20u16.to_le_bytes());
        central.extend(flags.to_le_bytes());
        central.extend(8u16.to_le_bytes());
        central.extend(time.to_le_bytes());
        central.extend(date.to_le_bytes());
        central.extend(crc.to_le_bytes());
        central.extend((compressed.len() as u32).to_le_bytes());
        central.extend((data.len() as u32).to_le_bytes());
        central.extend((name.len() as u16).to_le_bytes());
        central.extend([0u8; 8]);
        central.extend(attrs.to_le_bytes());
        central.extend(offset.to_le_bytes());
        central.extend(name);
    }
    let central_offset = zip.len() as u32;
    let central_len = central.len() as u32;
    zip.extend(central);
    zip.extend(0x06054b50u32.to_le_bytes());
    zip.extend([0u8; 4]);
    zip.extend((files.len() as u16).to_le_bytes());
    zip.extend((files.len() as u16).to_le_bytes());
    zip.extend(central_len.to_le_bytes());
    zip.extend(central_offset.to_le_bytes());
    zip.extend(0u16.to_le_bytes());
    crate::utils::debug::log_for_debugging(&format!(
        "Created ZIP from {}: {} files, {} bytes",
        source_dir.display(),
        files.len(),
        zip.len()
    ));
    Ok(zip)
}
/// Maps to: CC `utils/plugins/zipCache.ts:235-323#collectFilesForZip`.
/// BoxFuture is the native recursion-size/borrow carrier; source owns traversal.
fn collect_files_for_zip<'a>(
    base: &'a Path,
    relative: &'a str,
    files: &'a mut indexmap::IndexMap<String, ZipEntry>,
    visited: &'a mut std::collections::HashSet<String>,
) -> futures::future::BoxFuture<'a, ()> {
    Box::pin(async move {
        let current = if relative.is_empty() {
            base.to_path_buf()
        } else {
            zip_path!(base, relative)
        };
        let Ok(mut reader) = tokio::fs::read_dir(&current).await else {
            return;
        };
        let mut entries = Vec::new();
        loop {
            match reader.next_entry().await {
                Ok(Some(entry)) => entries.push(entry.file_name()),
                Ok(None) => break,
                Err(_) => return,
            }
        }
        entries.sort(); // Node/libuv readdir uses sorted scandir entries.
        let Ok(metadata) = tokio::fs::metadata(&current).await else {
            return;
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let (dev, ino) = (metadata.dev(), metadata.ino());
            if dev != 0 || ino != 0 {
                let key = format!("{dev}:{ino}");
                if !visited.insert(key) {
                    crate::utils::debug::log_for_debugging(&format!(
                        "Skipping symlink cycle at {}",
                        current.display()
                    ));
                    return;
                }
            }
        }
        #[cfg(windows)]
        {
            // Stable Rust does not expose dev/ino in MetadataExt. These Win32
            // fields carry Node stat({bigint:true}) identity without Number
            // rounding or a path-based surrogate for hard-linked directories.
            use std::os::windows::ffi::OsStrExt;
            #[repr(C)]
            struct FileInfo {
                attrs: u32,
                creation: [u32; 2],
                access: [u32; 2],
                write: [u32; 2],
                volume: u32,
                size_high: u32,
                size_low: u32,
                links: u32,
                index_high: u32,
                index_low: u32,
            }
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn CreateFileW(
                    name: *const u16,
                    access: u32,
                    share: u32,
                    security: *const std::ffi::c_void,
                    creation: u32,
                    flags: u32,
                    template: *mut std::ffi::c_void,
                ) -> *mut std::ffi::c_void;
                fn GetFileInformationByHandle(
                    handle: *mut std::ffi::c_void,
                    info: *mut FileInfo,
                ) -> i32;
                fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
            }
            let wide = current
                .as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>();
            let identity = unsafe {
                let handle = CreateFileW(
                    wide.as_ptr(),
                    0,
                    7,
                    std::ptr::null(),
                    3,
                    0x02000000,
                    std::ptr::null_mut(),
                );
                if handle as isize == -1 {
                    None
                } else {
                    let mut info = std::mem::MaybeUninit::<FileInfo>::uninit();
                    let ok = GetFileInformationByHandle(handle, info.as_mut_ptr());
                    CloseHandle(handle);
                    if ok == 0 {
                        None
                    } else {
                        let info = info.assume_init();
                        Some((
                            info.volume,
                            ((info.index_high as u64) << 32) | info.index_low as u64,
                        ))
                    }
                }
            };
            let Some((dev, ino)) = identity else { return };
            if dev != 0 || ino != 0 {
                let key = format!("{dev}:{ino}");
                if !visited.insert(key) {
                    crate::utils::debug::log_for_debugging(&format!(
                        "Skipping symlink cycle at {}",
                        current.display()
                    ));
                    return;
                }
            }
        }
        for entry in entries {
            if entry == ".git" {
                continue;
            }
            let full = zip_path!(&current, &entry);
            let name = entry.to_string_lossy();
            let rel = if relative.is_empty() {
                name.into_owned()
            } else {
                format!("{relative}/{name}")
            };
            let Ok(mut meta) = tokio::fs::symlink_metadata(&full).await else {
                continue;
            };
            if meta.file_type().is_symlink() {
                let Ok(target) = tokio::fs::metadata(&full).await else {
                    continue;
                };
                if target.is_dir() {
                    continue;
                }
                meta = target;
            }
            if meta.is_dir() {
                collect_files_for_zip(base, &rel, files, visited).await;
            } else if meta.is_file() {
                match tokio::fs::read(&full).await {
                    Ok(data) => {
                        #[cfg(unix)]
                        let mode = {
                            use std::os::unix::fs::MetadataExt;
                            meta.mode()
                        };
                        #[cfg(not(unix))]
                        let mode = if meta.permissions().readonly() {
                            0o100444
                        } else {
                            0o100666
                        };
                        files.insert(rel, (data, (mode & 0xffff) << 16));
                    }
                    Err(error) => crate::utils::debug::log_for_debugging(&format!(
                        "Failed to read file for zip: {rel}: {error}"
                    )),
                }
            }
        }
    })
}
/// Maps to: CC `utils/plugins/zipCache.ts:331-364#extractZipToDirectory`.
pub async fn extract_zip_to_directory(zip_path: &Path, target_dir: &Path) -> anyhow::Result<()> {
    let bytes = crate::utils::fs_operations::get_fs_implementation()
        .read_file_bytes(zip_path, None)
        .await?;
    let files = crate::utils::dxt::zip::unzip_file(&bytes).await?;
    let modes = crate::utils::dxt::zip::parse_zip_modes(&bytes);
    crate::utils::fs_operations::get_fs_implementation()
        .mkdir(&(target_dir), None)
        .await?;
    for (relative, data) in &files {
        if relative.ends_with('/') {
            crate::utils::fs_operations::get_fs_implementation()
                .mkdir(&(zip_path!(target_dir, relative)), None)
                .await?;
            continue;
        }
        let full = zip_path!(target_dir, relative);
        crate::utils::fs_operations::get_fs_implementation()
            .mkdir(&(zip_path!(&full, "..")), None)
            .await?;
        tokio::fs::write(&full, data).await?;
        if let Some(&mode) = modes.get(relative).filter(|&&m| m & 0o111 != 0) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = tokio::fs::set_permissions(
                    &full,
                    std::fs::Permissions::from_mode(mode & 0o777),
                )
                .await;
            }
            #[cfg(not(unix))]
            {
                let _ = mode;
            }
        }
    }
    crate::utils::debug::log_for_debugging(&format!(
        "Extracted ZIP to {}: {} entries",
        target_dir.display(),
        files.len()
    ));
    Ok(())
}
/// Maps to: CC `utils/plugins/zipCache.ts:371-378#convertDirectoryToZipInPlace`.
pub async fn convert_directory_to_zip_in_place(
    dir_path: &Path,
    zip_path: &Path,
) -> anyhow::Result<()> {
    let data = create_zip_from_directory(dir_path).await?;
    atomic_write_to_zip_cache(zip_path, &data).await?;
    crate::utils::fs_operations::native::rm(
        dir_path,
        crate::utils::fs_operations::RmOptions {
            recursive: true,
            force: true,
        },
    )
    .await?;
    Ok(())
}
/// Maps to: CC `utils/plugins/zipCache.ts:384-389#getMarketplaceJsonRelativePath`.
pub fn get_marketplace_json_relative_path(name: &str) -> PathBuf {
    let sanitized = name
        .encode_utf16()
        .map(|c| {
            if c < 128 && ((c as u8).is_ascii_alphanumeric() || c == 45 || c == 95) {
                char::from(c as u8)
            } else {
                '-'
            }
        })
        .collect::<String>();
    zip_path!("marketplaces", format!("{sanitized}.json"))
}
/// Maps to: CC `utils/plugins/zipCache.ts:402-406#isMarketplaceSourceSupportedByZipCache`.
pub fn is_marketplace_source_supported_by_zip_cache(source: &serde_json::Value) -> bool {
    matches!(
        source["source"].as_str(),
        Some("github" | "git" | "url" | "settings")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn zip_cache_roundtrip_preserves_files_modes_and_session_identity() {
        let root = std::env::temp_dir().join(format!("zip-cache-{}", uuid::Uuid::new_v4()));
        let input = root.join("input");
        tokio::fs::create_dir_all(input.join(".git")).await.unwrap();
        tokio::fs::write(input.join("hook"), b"#!/bin/sh\ntrue\n")
            .await
            .unwrap();
        tokio::fs::write(input.join(".git/skip"), b"skip")
            .await
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(input.join("hook"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
            std::os::unix::fs::symlink("hook", input.join("linked")).unwrap();
            std::os::unix::fs::symlink(&input, input.join("cycle")).unwrap();
        }
        let archive = create_zip_from_directory(&input).await.unwrap();
        let decoded = crate::utils::dxt::zip::unzip_file(&archive).await.unwrap();
        assert_eq!(decoded["hook"], b"#!/bin/sh\ntrue\n");
        assert!(!decoded.contains_key(".git/skip"));
        assert!(!decoded.contains_key("cycle/hook"));
        let zip = root.join("cache/archive.zip");
        atomic_write_to_zip_cache(&zip, &archive).await.unwrap();
        let output = root.join("output");
        extract_zip_to_directory(&zip, &output).await.unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(output.join("hook"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111,
                0o111
            );
            assert_eq!(
                std::fs::read(output.join("linked")).unwrap(),
                b"#!/bin/sh\ntrue\n"
            );
        }
        convert_directory_to_zip_in_place(&input, &root.join("converted.zip"))
            .await
            .unwrap();
        assert!(!input.exists());
        reset_session_plugin_cache();
        let (first, second) = tokio::join!(
            get_session_plugin_cache_path(),
            get_session_plugin_cache_path()
        );
        let first = first.unwrap();
        assert_eq!(first, second.unwrap());
        assert!(first.exists());
        cleanup_session_plugin_cache().await;
        assert!(!first.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn zip_cache_sources_and_utf16_sanitization_match_original() {
        assert_eq!(
            get_marketplace_json_relative_path("a/😀b"),
            PathBuf::from("marketplaces/a---b.json")
        );
        for (kind, expected) in [
            ("github", true),
            ("git", true),
            ("url", true),
            ("settings", true),
            ("file", false),
            ("directory", false),
            ("npm", false),
        ] {
            assert_eq!(
                is_marketplace_source_supported_by_zip_cache(&serde_json::json!({"source":kind})),
                expected
            );
        }
    }
}
