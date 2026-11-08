//! @nyan: the one decision-provider JSONL log (decision-provider §7).
//!
//! Every point appends one JSON object per decision to
//! `$NYAN_CONFIG_DIR/decision/decisions.jsonl`. Records are self-describing:
//! each carries `point`, `outcome` and, where applicable, `mode` and latency,
//! so `/decision` can aggregate them without knowing point-specific payloads.

use std::path::PathBuf;

use serde::Serialize;

/// `$NYAN_CONFIG_DIR/decision`.
pub fn log_dir() -> PathBuf {
    crate::utils::env_utils::get_config_home_dir().join("decision")
}

/// `$NYAN_CONFIG_DIR/decision/decisions.jsonl`.
pub fn log_path() -> PathBuf {
    log_dir().join("decisions.jsonl")
}

/// Append one record as a single JSON line.
pub fn append<T: Serialize>(record: &T) -> std::io::Result<()> {
    let path = log_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let line = serde_json::to_string(record)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    use std::io::Write;
    writeln!(file, "{line}")
}
