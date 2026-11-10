//! Maps to: CC `services/tips/types.ts` (usage-inferred Tip).

/// Maps to: CC `Tip` — spinner tip identity + already-resolved copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tip {
    pub id: String,
    pub content: String,
    pub cooldown_sessions: u64,
}
