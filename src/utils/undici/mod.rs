//! undici, the `fetch` dispatcher library CC's proxy agents are built on.
//!
//! Maps to: undici 7.24.6 (a dependency `../rebuild` consumes, not a CC source
//! file). Only the behavior CC's configuration observes is ported; reqwest
//! does the dispatching.

pub mod env_http_proxy_agent;
