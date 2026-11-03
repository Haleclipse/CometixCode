//! Provider credential references and provider-owned auth storage.
//!
//! `settings.nyan.providers.*.credential` only names where a secret comes
//! from. Nothing here writes a secret into settings. OAuth state lives in
//! `~/.nyan/auth/<provider>.json` (see `store`), and each OAuth provider owns
//! its own flow, refresh, and token schema.
//!
//! @nyan: this module does not consult `OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED`.
//! That switch guards only the inherited Anthropic/MCP OAuth outlets; provider
//! credential storage is a separate, deliberately enabled path
//! (docs/nyan/provider-redesign.md 4.2).

pub mod oauth;
pub mod reference;
pub mod refresh;
pub mod resolve;
pub mod store;
