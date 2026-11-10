pub const NAME: &str = "nyan";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Pinned upstream build marker. Outbound HTTP no longer sends it; the wire
/// User-Agent is [`crate::utils::http::user_agent`].
#[allow(dead_code)]
pub const USER_AGENT_VERSION: &str = "2.1.241";
pub const DESCRIPTION: &str = "Terminal-based AI coding assistant";
/// Maps to: CC `constants/product.ts#PRODUCT_URL`.
pub const PRODUCT_URL: &str = "https://github.com/blxzer77/nyan";
/// @nyan: in-repo design docs (`docs/nyan/`) on the default branch. CC links
/// to its hosted docs site, which describes a different product.
pub const DOCS_URL: &str = "https://github.com/blxzer77/nyan/blob/master/docs/nyan";
