//! Explicit clipboard-image ingestion for PromptInput.
//! Maps to: CC `utils/imagePaste.ts#getImageFromClipboard`.
//!
//! This is invoked only by the user-bound `chat:imagePaste` action. It reads
//! clipboard bytes through platform tools, performs no network access, and does
//! not write session/config state.

use base64::Engine as _;
use std::process::Command;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardImage {
    pub base64: String,
    pub media_type: String,
    /// Maps to: CC `getImageFromClipboard()` resized-image metadata.
    pub dimensions: Option<crate::utils::image_resizer::ImageDimensions>,
}

fn prepare_image_bytes(bytes: Vec<u8>, media_type: &str) -> ClipboardImage {
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    if let Ok(resized) = crate::utils::image_resizer::maybe_resize_and_downsample_image_base64(
        &encoded,
        Some(media_type),
    ) {
        return ClipboardImage {
            base64: crate::utils::image_resizer::resize_result_base64(&resized),
            media_type: resized.media_type,
            dimensions: resized.dimensions,
        };
    }
    ClipboardImage {
        base64: encoded,
        media_type: media_type.to_string(),
        dimensions: None,
    }
}

fn prepare_image_base64(data: String, media_type: &str) -> ClipboardImage {
    if let Ok(resized) = crate::utils::image_resizer::maybe_resize_and_downsample_image_base64(
        &data,
        Some(media_type),
    ) {
        return ClipboardImage {
            base64: crate::utils::image_resizer::resize_result_base64(&resized),
            media_type: resized.media_type,
            dimensions: resized.dimensions,
        };
    }
    ClipboardImage {
        base64: data,
        media_type: media_type.to_string(),
        dimensions: None,
    }
}

pub fn get_image_from_clipboard() -> Option<ClipboardImage> {
    #[cfg(target_os = "macos")]
    {
        return macos_clipboard_image();
    }
    #[cfg(target_os = "windows")]
    {
        return windows_clipboard_image();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return unix_clipboard_image();
    }
    #[allow(unreachable_code)]
    None
}

#[cfg(target_os = "macos")]
fn macos_clipboard_image() -> Option<ClipboardImage> {
    for (class, media_type) in [("PNGf", "image/png"), ("JPEG", "image/jpeg")] {
        let script =
            format!("try\nreturn the clipboard as «class {class}»\non error\nreturn \"\"\nend try");
        let mut command = Command::new("osascript");
        // CC inherits process.env (execa default); the carrier is its counterpart.
        crate::utils::subprocess_env::apply_process_env_std(&mut command);
        let output = command.args(["-e", &script]).output().ok()?;
        if !output.status.success() {
            continue;
        }
        let rendered = String::from_utf8_lossy(&output.stdout);
        let Some(data_start) = rendered.find(class).map(|index| index + class.len()) else {
            continue;
        };
        let hex = rendered[data_start..]
            .chars()
            .take_while(|ch| ch.is_ascii_hexdigit())
            .collect::<String>();
        if hex.len() < 2 || hex.len() % 2 != 0 {
            continue;
        }
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&hex[index..index + 2], 16))
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        return Some(prepare_image_bytes(bytes, media_type));
    }
    None
}

#[cfg(target_os = "windows")]
fn windows_clipboard_image() -> Option<ClipboardImage> {
    let script = r#"Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing; $i=[Windows.Forms.Clipboard]::GetImage(); if($null -ne $i){$m=New-Object IO.MemoryStream; $i.Save($m,[Drawing.Imaging.ImageFormat]::Png); [Convert]::ToBase64String($m.ToArray())}"#;
    let mut command = Command::new("powershell.exe");
    // CC inherits process.env (execa default); the carrier is its counterpart.
    crate::utils::subprocess_env::apply_process_env_std(&mut command);
    let output = command
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .ok()?;
    let data = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!data.is_empty()).then(|| prepare_image_base64(data, "image/png"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn unix_clipboard_image() -> Option<ClipboardImage> {
    for (program, args, media_type) in [
        (
            "wl-paste",
            vec!["--no-newline", "--type", "image/png"],
            "image/png",
        ),
        (
            "wl-paste",
            vec!["--no-newline", "--type", "image/jpeg"],
            "image/jpeg",
        ),
        (
            "xclip",
            vec!["-selection", "clipboard", "-t", "image/png", "-o"],
            "image/png",
        ),
    ] {
        let mut command = Command::new(program);
        // CC inherits process.env (execa default); the carrier is its counterpart.
        crate::utils::subprocess_env::apply_process_env_std(&mut command);
        let Ok(output) = command.args(args).output() else {
            continue;
        };
        if output.status.success() && !output.stdout.is_empty() {
            return Some(prepare_image_bytes(output.stdout, media_type));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_image_payload_is_typed_for_prompt_runtime() {
        let image = ClipboardImage {
            base64: "AAAA".to_string(),
            media_type: "image/png".to_string(),
            dimensions: None,
        };
        assert_eq!(image.media_type, "image/png");
    }
}
