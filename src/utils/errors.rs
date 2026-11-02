//! Maps to: CC `utils/errors.ts`.
//!
//! Configuration parse errors and abort classification are ported here.
//! Other official error classes remain to be added with their owning slices.

/// Maps to: CC `utils/errors.ts` `AbortError`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbortError {
    pub message: String,
}

impl AbortError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Default for AbortError {
    fn default() -> Self {
        Self::new("AbortError")
    }
}

impl std::fmt::Display for AbortError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AbortError {}

/// Maps to: CC `utils/errors.ts:51-61` `ShellError`.
///
/// Carries the shell result a failed command produced. CC's constructor sets
/// the message to the fixed `"Shell command failed"`; the parts a consumer
/// actually renders (`Exit code N`, interruption notice, stderr, stdout) are
/// assembled by `tool_errors::get_error_parts`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellError {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
    pub interrupted: bool,
}

impl ShellError {
    pub fn new(
        stdout: impl Into<String>,
        stderr: impl Into<String>,
        code: i32,
        interrupted: bool,
    ) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: stderr.into(),
            code,
            interrupted,
        }
    }
}

impl std::fmt::Display for ShellError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // CC: `super('Shell command failed')`.
        write!(f, "Shell command failed")
    }
}

impl std::error::Error for ShellError {}

/// Maps to: CC `utils/errors.ts` `isAbortError` — true for our [`AbortError`],
/// SDK `APIUserAbortError` (`ApiError::UserAbort`), or errors named `AbortError`.
///
/// Also treats `with_retry::RetryableError::Aborted` (and its
/// `CannotRetryError` wrapper) as abort: Rust `with_retry` returns that shape
/// where TS `withRetry` throws `APIUserAbortError` directly.
pub fn is_abort_error(error: &anyhow::Error) -> bool {
    for cause in error.chain() {
        if cause.downcast_ref::<AbortError>().is_some()
            || cause
                .downcast_ref::<crate::utils::read_file_in_range::ReadFileInRangeError>()
                .is_some_and(|error| {
                    matches!(
                        error,
                        crate::utils::read_file_in_range::ReadFileInRangeError::AbortedBeforeIo
                            | crate::utils::read_file_in_range::ReadFileInRangeError::Aborted
                    )
                })
        {
            return true;
        }
        if cause
            .downcast_ref::<anthropic_sdk::ApiError>()
            .is_some_and(|api| matches!(api, anthropic_sdk::ApiError::UserAbort { .. }))
        {
            return true;
        }
        if let Some(cannot_retry) =
            cause.downcast_ref::<crate::services::api::with_retry::CannotRetryError>()
            && matches!(
                &cannot_retry.original_error,
                crate::services::api::with_retry::RetryableError::Aborted
            )
        {
            return true;
        }
        if cause
            .downcast_ref::<crate::services::api::with_retry::RetryableError>()
            .is_some_and(|e| matches!(e, crate::services::api::with_retry::RetryableError::Aborted))
        {
            return true;
        }
    }
    false
}

/// Maps to: CC `utils/errors.ts:128-133` `getErrnoCode` for the native Rust
/// error identities exercised by filesystem tools.
pub fn get_errno_code(error: &anyhow::Error) -> Option<&'static str> {
    for cause in error.chain() {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return io_errno_code(io);
        }
        if let Some(range) =
            cause.downcast_ref::<crate::utils::read_file_in_range::ReadFileInRangeError>()
            && let Some(io) = range.io_error()
        {
            return io_errno_code(io);
        }
    }
    None
}

pub(crate) fn io_errno_code(error: &std::io::Error) -> Option<&'static str> {
    if let Some(fs_error) = error
        .get_ref()
        .and_then(|error| error.downcast_ref::<crate::utils::fs_operations::FsError>())
    {
        return Some(fs_error.code);
    }
    #[cfg(unix)]
    if let Some(raw) = error.raw_os_error() {
        return Some(match raw {
            libc::ENOENT => "ENOENT",
            libc::EACCES => "EACCES",
            libc::EPERM => "EPERM",
            libc::EEXIST => "EEXIST",
            libc::ENOTDIR => "ENOTDIR",
            libc::EISDIR => "EISDIR",
            libc::ELOOP => "ELOOP",
            libc::ENAMETOOLONG => "ENAMETOOLONG",
            libc::ENOSPC => "ENOSPC",
            libc::EROFS => "EROFS",
            libc::EMFILE => "EMFILE",
            libc::ENFILE => "ENFILE",
            libc::EFBIG => "EFBIG",
            libc::EBADF => "EBADF",
            libc::EINVAL => "EINVAL",
            libc::EIO => "EIO",
            libc::EINTR => "EINTR",
            _ => return errno_code_for_kind(error.kind()),
        });
    }
    errno_code_for_kind(error.kind())
}

fn errno_code_for_kind(kind: std::io::ErrorKind) -> Option<&'static str> {
    Some(match kind {
        std::io::ErrorKind::NotFound => "ENOENT",
        std::io::ErrorKind::PermissionDenied => "EACCES",
        std::io::ErrorKind::AlreadyExists => "EEXIST",
        std::io::ErrorKind::NotADirectory => "ENOTDIR",
        std::io::ErrorKind::IsADirectory => "EISDIR",
        _ => return None,
    })
}

/// Native Rust projection of Node's path/operation-bearing filesystem error
/// display. Errno classification remains based on the typed I/O error rather
/// than localized message matching.
pub(crate) fn format_native_file_error(
    error: &std::io::Error,
    operation: &str,
    path: Option<&std::path::Path>,
) -> String {
    let suffix = path
        .map(|path| format!(" '{}'", path.display()))
        .unwrap_or_default();
    let code = io_errno_code(error).unwrap_or("EIO");
    let message = match code {
        "EPERM" => "operation not permitted",
        "ENAMETOOLONG" => "name too long",
        "ENOSPC" => "no space left on device",
        "EROFS" => "read-only file system",
        "EMFILE" => "too many open files",
        "ENFILE" => "file table overflow",
        "EFBIG" => "file too large",
        "EBADF" => "bad file descriptor",
        "EINVAL" => "invalid argument",
        "EINTR" => "interrupted system call",
        "EIO" if error.raw_os_error() == Some(libc::EIO) => "i/o error",
        "ELOOP" => "too many symbolic links encountered",
        _ => match error.kind() {
            std::io::ErrorKind::NotFound => "no such file or directory",
            std::io::ErrorKind::PermissionDenied => "permission denied",
            std::io::ErrorKind::AlreadyExists => "file already exists",
            std::io::ErrorKind::NotADirectory => "not a directory",
            std::io::ErrorKind::IsADirectory => "illegal operation on a directory",
            _ => return format!("{code}: {error}, {operation}{suffix}"),
        },
    };
    format!("{code}: {message}, {operation}{suffix}")
}

/// Maps to: CC `utils/errors.ts:139-141` `isENOENT`.
pub fn is_enoent(error: &anyhow::Error) -> bool {
    get_errno_code(error) == Some("ENOENT")
}

/// Maps to: CC `utils/errors.ts:186-195` `isFsInaccessible`.
/// Native I/O errors carry Node's `code` through the existing errno projection.
pub fn is_fs_inaccessible(error: &std::io::Error) -> bool {
    matches!(
        io_errno_code(error),
        Some("ENOENT" | "EACCES" | "EPERM" | "ENOTDIR" | "ELOOP")
    )
}

/// Maps to: CC `utils/errors.ts` `ConfigParseError`.
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigParseError {
    pub message: String,
    pub file_path: String,
    pub default_config: serde_json::Value,
}

impl ConfigParseError {
    pub fn new(
        message: impl Into<String>,
        file_path: impl Into<String>,
        default_config: serde_json::Value,
    ) -> Self {
        Self {
            message: message.into(),
            file_path: file_path.into(),
            default_config,
        }
    }
}

impl std::fmt::Display for ConfigParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ConfigParseError {}

/// Maps to: CC `utils/errors.ts:111-113` `toError`.
/// L1 preserves existing Error fields; other JS values become a built-in Error
/// using the canonical String conversion and a native stack carrier.
pub fn to_error(error: crate::utils::log::McpLogError) -> crate::utils::log::LogError {
    match error {
        crate::utils::log::McpLogError::Error(error) => error,
        other => crate::utils::log::LogError::new(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn is_fs_inaccessible_matches_official_errno_set() {
        // CC errors.ts:186-195: only these five codes are recoverable reads.
        for errno in [
            libc::ENOENT,
            libc::EACCES,
            libc::EPERM,
            libc::ENOTDIR,
            libc::ELOOP,
        ] {
            assert!(is_fs_inaccessible(&std::io::Error::from_raw_os_error(
                errno
            )));
        }
        for errno in [libc::EISDIR, libc::EIO, libc::EEXIST, libc::ENOSPC] {
            assert!(!is_fs_inaccessible(&std::io::Error::from_raw_os_error(
                errno
            )));
        }
    }

    #[test]
    fn config_parse_error_preserves_official_fields() {
        let error = ConfigParseError::new(
            "Unexpected token",
            "/tmp/config.json",
            serde_json::json!({ "theme": "dark" }),
        );

        assert_eq!(error.to_string(), "Unexpected token");
        assert_eq!(error.file_path, "/tmp/config.json");
        assert_eq!(error.default_config["theme"], "dark");
    }

    #[test]
    fn errno_helpers_match_official_typed_native_errors() {
        let direct = anyhow::Error::new(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "localized text is irrelevant",
        ));
        assert_eq!(get_errno_code(&direct), Some("ENOENT"));
        assert!(is_enoent(&direct));

        let range = anyhow::Error::new(crate::utils::read_file_in_range::ReadFileInRangeError::Io(
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "localized"),
        ));
        assert_eq!(get_errno_code(&range), Some("EACCES"));
        assert!(!is_enoent(&range));
    }

    #[test]
    fn is_abort_error_matches_abort_error_and_sdk_user_abort() {
        assert!(is_abort_error(&anyhow::Error::new(AbortError::default())));
        assert!(is_abort_error(&anyhow::Error::new(
            crate::utils::read_file_in_range::ReadFileInRangeError::AbortedBeforeIo,
        )));
        assert!(is_abort_error(&anyhow::Error::new(
            anthropic_sdk::ApiError::UserAbort {
                message: "Request was aborted.".to_owned(),
            },
        )));
        assert!(!is_abort_error(&anyhow::anyhow!("prompt is too long")));
    }
    #[test]
    fn to_error_matches_official_existing_identity_and_unknown_coercion() {
        use crate::utils::log::{LogError, McpLogError};
        let original = LogError {
            name: "TypeError".into(),
            message: "bad".into(),
            stack: Some("original stack".into()),
            axios: None,
        };
        assert_eq!(to_error(McpLogError::Error(original.clone())), original);
        let object = to_error(McpLogError::Value(
            serde_json::json!({"message":"not Error"}),
        ));
        assert_eq!(object.name, "Error");
        assert_eq!(object.message, "[object Object]");
        assert!(
            object
                .stack
                .as_deref()
                .unwrap()
                .starts_with("Error: [object Object]")
        );
        assert_eq!(to_error(McpLogError::Undefined).message, "undefined");
    }
}
