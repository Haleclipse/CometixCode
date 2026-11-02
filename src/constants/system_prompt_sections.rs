//! Session-scoped system-prompt section registry.
//!
//! Maps to: CC `constants/systemPromptSections.ts:1-69`.
//!
//! CC stores the cache in `bootstrap/state.ts`; Rust keeps the same owner and
//! exposes lock-safe entry accessors rather than returning a mutable global map.

/// Deferred system-prompt section computation.
///
/// Maps to: CC `constants/systemPromptSections.ts:8-12`
/// `SystemPromptSection`.
pub struct SystemPromptSection<'a> {
    name: &'static str,
    compute: Box<dyn Fn() -> Option<String> + 'a>,
    cache_break: bool,
}

/// Maps to: CC `constants/systemPromptSections.ts:18-23`
/// `systemPromptSection(...)`.
pub fn system_prompt_section<'a, F>(name: &'static str, compute: F) -> SystemPromptSection<'a>
where
    F: Fn() -> Option<String> + 'a,
{
    SystemPromptSection {
        name,
        compute: Box::new(compute),
        cache_break: false,
    }
}

/// Maps to: CC `constants/systemPromptSections.ts:32-41`
/// `DANGEROUS_uncachedSystemPromptSection(...)`.
pub fn dangerous_uncached_system_prompt_section<'a, F>(
    name: &'static str,
    compute: F,
    _reason: &'static str,
) -> SystemPromptSection<'a>
where
    F: Fn() -> Option<String> + 'a,
{
    SystemPromptSection {
        name,
        compute: Box::new(compute),
        cache_break: true,
    }
}

/// Maps to: CC `constants/systemPromptSections.ts:46-60`
/// `resolveSystemPromptSections(...)`.
pub fn resolve_system_prompt_sections(
    sections: Vec<SystemPromptSection<'_>>,
) -> Vec<Option<String>> {
    sections
        .into_iter()
        .map(|section| {
            if !section.cache_break
                && let Some(cached) =
                    crate::bootstrap::state::get_system_prompt_section_cache_entry(section.name)
            {
                return cached;
            }
            let value = (section.compute)();
            crate::bootstrap::state::set_system_prompt_section_cache_entry(
                section.name,
                value.clone(),
            );
            value
        })
        .collect()
}

/// Maps to: CC `constants/systemPromptSections.ts:65-68`
/// `clearSystemPromptSections()`.
pub fn clear_system_prompt_sections() {
    crate::bootstrap::state::clear_system_prompt_section_state();
    // CC also resets beta-header latches here. Nyan does not yet own those
    // API-header latches, so there is no disconnected state to clear.
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn cached_and_uncached_sections_match_official_resolution_contract() {
        clear_system_prompt_sections();
        let cached_calls = Cell::new(0usize);
        let uncached_calls = Cell::new(0usize);

        let first = resolve_system_prompt_sections(vec![
            system_prompt_section("cached", || {
                cached_calls.set(cached_calls.get() + 1);
                Some(format!("cached-{}", cached_calls.get()))
            }),
            dangerous_uncached_system_prompt_section(
                "uncached",
                || {
                    uncached_calls.set(uncached_calls.get() + 1);
                    Some(format!("uncached-{}", uncached_calls.get()))
                },
                "test value changes between turns",
            ),
        ]);
        let second = resolve_system_prompt_sections(vec![
            system_prompt_section("cached", || {
                cached_calls.set(cached_calls.get() + 1);
                Some(format!("cached-{}", cached_calls.get()))
            }),
            dangerous_uncached_system_prompt_section(
                "uncached",
                || {
                    uncached_calls.set(uncached_calls.get() + 1);
                    Some(format!("uncached-{}", uncached_calls.get()))
                },
                "test value changes between turns",
            ),
        ]);

        assert_eq!(
            first,
            vec![Some("cached-1".into()), Some("uncached-1".into())]
        );
        assert_eq!(
            second,
            vec![Some("cached-1".into()), Some("uncached-2".into())]
        );
        assert_eq!(cached_calls.get(), 1);
        assert_eq!(uncached_calls.get(), 2);
        clear_system_prompt_sections();
    }

    #[test]
    fn cached_none_is_distinct_from_a_missing_section_and_clear_rearms_it() {
        clear_system_prompt_sections();
        let calls = Cell::new(0usize);
        let resolve = || {
            resolve_system_prompt_sections(vec![system_prompt_section("nullable", || {
                calls.set(calls.get() + 1);
                None
            })])
        };

        assert_eq!(resolve(), vec![None]);
        assert_eq!(resolve(), vec![None]);
        assert_eq!(calls.get(), 1);
        assert_eq!(
            crate::bootstrap::state::system_prompt_section_cache_len_for_test(),
            1
        );

        clear_system_prompt_sections();
        assert_eq!(resolve(), vec![None]);
        assert_eq!(calls.get(), 2);
        clear_system_prompt_sections();
    }
}
