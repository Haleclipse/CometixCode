//! Decision backends.
//!
//! One protocol: System One (decision-provider.md §5). [`system_one`] holds
//! the reusable client and transport seam; [`emrank`] is the provider built on
//! it. There is no chat backend; an unavailable EMRank falls back to the
//! deterministic path.

pub mod emrank;
pub mod system_one;
