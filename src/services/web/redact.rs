//! Provider error/log redaction.

pub fn redact_error(raw: &str, secrets: &[String]) -> String {
    let mut value = raw.replace(['\n', '\r'], " ");
    for secret in secrets {
        let secret = secret.trim();
        if !secret.is_empty() {
            value = value.replace(secret, "<redacted>");
        }
    }
    value.chars().take(200).collect()
}
