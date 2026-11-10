//! Maps to: CC `utils/permissions/dangerousPatterns.ts`.
//!
//! Shared dangerous shell prefix lists used by auto-mode permission setup.
//! Callers should use `dangerous_bash_patterns()` rather than assuming the
//! cross-platform slice is exhaustive.

/// Maps to: CC `CROSS_PLATFORM_CODE_EXEC`.
pub const CROSS_PLATFORM_CODE_EXEC: &[&str] = &[
    "python", "python3", "python2", "node", "deno", "tsx", "ruby", "perl", "php", "lua", "npx",
    "bunx", "npm run", "yarn run", "pnpm run", "bun run", "bash", "sh", "ssh",
];

const DANGEROUS_BASH_BASE_TAIL: &[&str] = &["zsh", "fish", "eval", "exec", "env", "xargs", "sudo"];

/// Maps to: CC `DANGEROUS_BASH_PATTERNS`.
pub fn dangerous_bash_patterns() -> Vec<&'static str> {
    let mut patterns =
        Vec::with_capacity(CROSS_PLATFORM_CODE_EXEC.len() + DANGEROUS_BASH_BASE_TAIL.len());
    patterns.extend_from_slice(CROSS_PLATFORM_CODE_EXEC);
    patterns.extend_from_slice(DANGEROUS_BASH_BASE_TAIL);
    patterns
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dangerous_patterns_include_cross_platform_and_shell_entries() {
        let patterns = dangerous_bash_patterns();
        assert!(patterns.contains(&"python"));
        assert!(patterns.contains(&"npm run"));
        assert!(patterns.contains(&"sudo"));
        assert!(!patterns.contains(&"gh api"));
    }
}
