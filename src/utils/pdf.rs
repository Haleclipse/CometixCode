//! PDF read / page-extraction helpers.
//!
//! Maps to: CC `utils/pdf.ts:1-300`.
//!
//! Full-document path is pure filesystem + base64 (no crate).
//! Page rendering shells out to system `pdftoppm` / `pdfinfo` from
//! poppler-utils — same dependency surface as upstream Claude Code.

use crate::constants::api_limits::{PDF_MAX_EXTRACT_SIZE, PDF_TARGET_RAW_SIZE};
use crate::utils::exec_file_no_throw::exec_file_no_throw;
use crate::utils::format::format_file_size;
use crate::utils::pdf_utils::PdfPageRange;
use crate::utils::tool_result_storage::get_tool_results_dir;
use base64::Engine as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

/// Maps to: CC `utils/pdf.ts:13-22` `PDFError.reason`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdfErrorReason {
    Empty,
    TooLarge,
    PasswordProtected,
    Corrupted,
    Unknown,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfError {
    pub reason: PdfErrorReason,
    pub message: String,
}

pub type PdfResult<T> = Result<T, PdfError>;

/// Maps to: CC `utils/pdf.ts:34-113` `readPDF` success payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfDocumentData {
    pub file_path: String,
    pub base64: String,
    pub original_size: u64,
}

/// Maps to: CC `utils/pdf.ts:137-145` `PDFExtractPagesResult`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfExtractPagesData {
    pub file_path: String,
    pub original_size: u64,
    pub count: usize,
    pub output_dir: PathBuf,
}

/// Maps to: CC `utils/pdf.ts:34-113` `readPDF(filePath)`.
pub fn read_pdf(
    file_path: &Path,
) -> impl std::future::Future<Output = PdfResult<PdfDocumentData>> + '_ {
    let metadata = crate::utils::fs_operations::get_fs_implementation().stat(file_path);
    async move {
        let metadata = metadata.await.map_err(|error| PdfError {
            reason: PdfErrorReason::Unknown,
            message: error.to_string(),
        })?;
        let original_size = metadata.len();
        if original_size == 0 {
            return Err(PdfError {
                reason: PdfErrorReason::Empty,
                message: format!("PDF file is empty: {}", file_path.display()),
            });
        }
        if original_size > PDF_TARGET_RAW_SIZE {
            return Err(PdfError {
                reason: PdfErrorReason::TooLarge,
                message: format!(
                    "PDF file exceeds maximum allowed size of {}.",
                    format_file_size(PDF_TARGET_RAW_SIZE)
                ),
            });
        }
        let bytes = std::fs::read(file_path).map_err(|error| PdfError {
            reason: PdfErrorReason::Unknown,
            message: error.to_string(),
        })?;
        let header = String::from_utf8_lossy(&bytes[..bytes.len().min(5)]);
        if !header.starts_with("%PDF-") {
            return Err(PdfError {
                reason: PdfErrorReason::Corrupted,
                message: format!(
                    "File is not a valid PDF (missing %PDF- header): {}",
                    file_path.display()
                ),
            });
        }

        Ok(PdfDocumentData {
            file_path: file_path.display().to_string(),
            base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
            original_size,
        })
    }
}

/// Maps to: CC `utils/pdf.ts:119-135` `getPDFPageCount(filePath)`.
pub fn get_pdf_page_count(file_path: &Path) -> Option<u64> {
    let output = exec_file_no_throw(
        "pdfinfo",
        &[file_path.to_string_lossy().as_ref()],
        Duration::from_secs(10),
    );
    if output.code != 0 {
        return None;
    }
    let re = regex::Regex::new(r"(?m)^Pages:\s+(\d+)").ok()?;
    let caps = re.captures(&output.stdout)?;
    caps.get(1)?.as_str().parse().ok()
}

// 0 = unset, 1 = available, 2 = unavailable
static PDFTOPPM_CACHE: AtomicU8 = AtomicU8::new(0);

/// Maps to: CC `utils/pdf.ts:150-155` `resetPdftoppmCache` (tests).
#[cfg(test)]
pub fn reset_pdftoppm_cache() {
    PDFTOPPM_CACHE.store(0, Ordering::SeqCst);
}

/// Maps to: CC `utils/pdf.ts:157-171` `isPdftoppmAvailable()`.
pub fn is_pdftoppm_available() -> bool {
    match PDFTOPPM_CACHE.load(Ordering::SeqCst) {
        1 => return true,
        2 => return false,
        _ => {}
    }
    let output = exec_file_no_throw("pdftoppm", &["-v"], Duration::from_secs(5));
    let available = output.code == 0 || !output.stderr.is_empty();
    PDFTOPPM_CACHE.store(if available { 1 } else { 2 }, Ordering::SeqCst);
    available
}

/// Maps to: CC `utils/pdf.ts:179-300` `extractPDFPages(filePath, options)`.
pub fn extract_pdf_pages(
    file_path: &Path,
    range: Option<PdfPageRange>,
) -> impl std::future::Future<Output = PdfResult<PdfExtractPagesData>> + '_ {
    let metadata = crate::utils::fs_operations::get_fs_implementation().stat(file_path);
    async move {
        let metadata = metadata.await.map_err(|error| PdfError {
            reason: PdfErrorReason::Unknown,
            message: error.to_string(),
        })?;
        let original_size = metadata.len();
        if original_size == 0 {
            return Err(PdfError {
                reason: PdfErrorReason::Empty,
                message: format!("PDF file is empty: {}", file_path.display()),
            });
        }
        if original_size > PDF_MAX_EXTRACT_SIZE {
            return Err(PdfError {
                reason: PdfErrorReason::TooLarge,
                message: format!(
                    "PDF file exceeds maximum allowed size for text extraction ({}).",
                    format_file_size(PDF_MAX_EXTRACT_SIZE)
                ),
            });
        }
        if !is_pdftoppm_available() {
            return Err(PdfError {
            reason: PdfErrorReason::Unavailable,
            message: "pdftoppm is not installed. Install poppler-utils (e.g. `brew install poppler` or `apt-get install poppler-utils`) to enable PDF page rendering.".to_string(),
        });
        }

        let uuid = uuid::Uuid::new_v4();
        let output_dir = get_tool_results_dir().join(format!("pdf-{uuid}"));
        std::fs::create_dir_all(&output_dir).map_err(|error| PdfError {
            reason: PdfErrorReason::Unknown,
            message: error.to_string(),
        })?;
        let prefix = output_dir.join("page");
        let mut args = vec!["-jpeg".to_string(), "-r".to_string(), "100".to_string()];
        if let Some(range) = range {
            args.push("-f".to_string());
            args.push(ryu_js::Buffer::new().format(range.first_page).to_string());
            if let Some(last_page) = range.last_page {
                args.push("-l".to_string());
                args.push(ryu_js::Buffer::new().format(last_page).to_string());
            }
        }
        args.push(file_path.to_string_lossy().into_owned());
        args.push(prefix.to_string_lossy().into_owned());

        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = exec_file_no_throw("pdftoppm", &arg_refs, Duration::from_secs(120));

        if output.code != 0 {
            let stderr = output.stderr;
            if regex::Regex::new(r"(?i)password")
                .ok()
                .is_some_and(|re| re.is_match(&stderr))
            {
                return Err(PdfError {
                    reason: PdfErrorReason::PasswordProtected,
                    message: "PDF is password-protected. Please provide an unprotected version."
                        .to_string(),
                });
            }
            if regex::Regex::new(r"(?i)damaged|corrupt|invalid")
                .ok()
                .is_some_and(|re| re.is_match(&stderr))
            {
                return Err(PdfError {
                    reason: PdfErrorReason::Corrupted,
                    message: "PDF file is corrupted or invalid.".to_string(),
                });
            }
            return Err(PdfError {
                reason: PdfErrorReason::Unknown,
                message: format!("pdftoppm failed: {stderr}"),
            });
        }

        let entries = std::fs::read_dir(&output_dir)
            .map_err(|error| PdfError {
                reason: PdfErrorReason::Unknown,
                message: error.to_string(),
            })?
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|error| PdfError {
                reason: PdfErrorReason::Unknown,
                message: error.to_string(),
            })?;
        let mut image_files = entries
            .into_iter()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.ends_with(".jpg"))
            })
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        image_files.sort();
        if image_files.is_empty() {
            return Err(PdfError {
                reason: PdfErrorReason::Corrupted,
                message: "pdftoppm produced no output pages. The PDF may be invalid.".to_string(),
            });
        }

        Ok(PdfExtractPagesData {
            file_path: file_path.display().to_string(),
            original_size,
            count: image_files.len(),
            output_dir,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn read_pdf_validation_and_poppler_errors_matches_official_contract() {
        let dir = std::env::temp_dir().join(format!("nyan-pdf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let empty = dir.join("empty.pdf");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(
            read_pdf(&empty).await.unwrap_err().reason,
            PdfErrorReason::Empty
        );

        let fake = dir.join("fake.pdf");
        std::fs::write(&fake, b"<html>not pdf</html>").unwrap();
        assert_eq!(
            read_pdf(&fake).await.unwrap_err().reason,
            PdfErrorReason::Corrupted
        );

        let ok = dir.join("ok.pdf");
        std::fs::write(&ok, b"%PDF-1.4 minimal").unwrap();
        let data = read_pdf(&ok).await.expect("valid header");
        assert!(!data.base64.is_empty());

        if is_pdftoppm_available() {
            let page = dir.join("page.pdf");
            std::fs::write(
                &page,
                b"%PDF-1.1\n\
1 0 obj<< /Type /Catalog /Pages 2 0 R >>endobj\n\
2 0 obj<< /Type /Pages /Kids [3 0 R] /Count 1 >>endobj\n\
3 0 obj<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>endobj\n\
xref\n0 4\n0000000000 65535 f \n0000000009 00000 n \n0000000058 00000 n \n0000000115 00000 n \n\
trailer<< /Size 4 /Root 1 0 R >>\nstartxref\n190\n%%EOF\n",
            )
            .unwrap();
            assert_eq!(get_pdf_page_count(&page), Some(1));
            match extract_pdf_pages(
                &page,
                Some(crate::utils::pdf_utils::PdfPageRange {
                    first_page: 1.0,
                    last_page: Some(1.0),
                }),
            )
            .await
            {
                Ok(extract) => {
                    assert_eq!(extract.count, 1);
                    let _ = std::fs::remove_dir_all(&extract.output_dir);
                }
                Err(err)
                    if err.message.contains("Operation not permitted")
                        || err.reason == PdfErrorReason::Unavailable => {}
                Err(err) => panic!("extract: {err:?}"),
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
