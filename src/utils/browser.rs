//! Local browser/path opener helpers.
//! Maps to: CC `utils/browser.ts`.

use std::path::Path;
use std::process::{Command, Stdio};

/// Maps to: CC `utils/browser.ts:39-70` `openBrowser`.
///
/// Deviation (L2, user-authorized OAuth safety gate): the only currently
/// ported `openBrowser` call sites launch OAuth authorization URLs. URL,
/// environment, platform, executable, arguments, and stdio are fully prepared
/// before the canonical product gate rejects the final process spawn. `openPath`
/// remains available for unrelated local-path launches.
pub async fn open_browser(url: &str) -> anyhow::Result<bool> {
    let Ok(parsed_url) = reqwest::Url::parse(url) else {
        return Ok(false);
    };
    if !matches!(parsed_url.scheme(), "http" | "https") {
        return Ok(false);
    }

    let browser = crate::utils::process_env::var_os("BROWSER").filter(|value| !value.is_empty());
    let mut command = if cfg!(target_os = "windows") {
        if let Some(browser) = browser {
            let mut command = Command::new(browser);
            command.arg(format!("\"{url}\""));
            command
        } else {
            let mut command = Command::new("rundll32");
            command.args(["url,OpenURL", url]);
            command
        }
    } else {
        let executable = browser.unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                "open".into()
            } else {
                "xdg-open".into()
            }
        });
        let mut command = Command::new(executable);
        command.arg(url);
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
        return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
    }
    Ok(command.spawn().is_ok())
}

/// Maps to: CC `utils/browser.ts:24-37` `openPath`.
///
/// This is only invoked after an explicit user selection. It does not wait for
/// the child application and performs no network or shell evaluation.
pub fn open_path(path: &Path) -> std::io::Result<()> {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("open");
        command.arg(path);
        command
    } else if cfg!(target_os = "windows") {
        let mut command = Command::new("explorer");
        command.arg(path);
        command
    } else {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_path_uses_direct_process_arguments_without_a_shell() {
        // Compile-time/API-shape guard: callers provide a Path, and the
        // implementation never formats a shell command. Do not launch a GUI
        // application from tests.
        let _signature: fn(&Path) -> std::io::Result<()> = open_path;
    }

    #[tokio::test]
    async fn oauth_browser_preparation_is_source_shaped_and_spawn_is_default_closed() {
        assert!(!open_browser("file:///tmp/token").await.unwrap());
        assert!(!open_browser("not a URL").await.unwrap());
        let error = open_browser("https://example.com/oauth")
            .await
            .expect_err("OAuth browser process spawn must be default-closed");
        assert!(
            error
                .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>()
                .is_some()
        );
    }
}
