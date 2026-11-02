//! Maps to: fuse.js v7 (the dependency `../rebuild` consumes, not a CC source
//! file — same classification as `utils/zod`).
//!
//! CC lists `"fuse.js": "^7.0.0"` in `package.json` and imports it at three
//! sites —
//!
//!   - `hooks/unifiedSuggestions.ts:174-192` (MCP resources + agents)
//!   - `utils/suggestions/commandSuggestions.ts:53-76` (slash commands)
//!   - `components/LogSelector.tsx:301-306` (session logs)
//!
//! so the Rust equivalent is one module the three call sites share, exactly as
//! the three TS sites share one import. Before this module existed each site
//! had grown its own `fuse_like_score`, which is how they drifted apart:
//! `commandSuggestions` honoured the per-key weights, `LogSelector` matched
//! `ignoreLocation`, and `unifiedSuggestions` had neither.
//!
//! Names follow the library rather than this codebase, so the mapping can be
//! checked against `node_modules/fuse.js/dist/fuse.js`: [`KeyStore`] (:255),
//! [`BitapSearch`] (:894), [`BitapSearch::search_in`] (`searchIn`, :961),
//! [`BitapSearch::compute_score`] (`computeScore`, :1670), and the
//! `location` / `distance` / `threshold` / `ignoreLocation` options (:400-426).
//!
//! # What is reproduced, and what is not
//!
//! Faithful: the 0..1 lower-is-better score; `KeyStore`'s configure-time weight
//! normalization; `computeScore`'s weighted geometric product with the
//! `Number.EPSILON` substitution; the field-length norm; `threshold` as a
//! cutoff; and `computeScore$1`'s `accuracy + proximity / distance` for exact
//! and substring hits.
//!
//! **Not** faithful: the approximate-match core. Fuse's `BitapSearch` is bitap
//! over an edit-distance window; nucleo is fzf-style subsequence matching, so a
//! pattern with transposed or missing characters scores differently. One
//! consequence is scoped and deliberate: nucleo does not hand back a match
//! position from `score()`, so the *fuzzy* branch carries its location bias
//! through nucleo's own `prefer_prefix` rather than through the
//! `proximity / distance` term. Exact and substring hits — the common path for
//! command names, agent types and MCP names — do use the real formula.

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Maps to Fuse's `KeyStore` (`fuse.js:255-272`): the configured `keys` table,
/// summed and normalized **once at construction** — `key.weight /= totalWeight`.
///
/// The divisor is the full configured table, never the subset a given item
/// happens to populate. `unifiedSuggestions` configures five keys totalling 10,
/// so `name` is `3/10` whether it is scoring an MCP row (which has four of the
/// five) or an agent row (three). Re-normalizing per item would make the same
/// key `3/7` and `3/6` respectively, silently changing the ranking between two
/// source shapes that Fuse treats identically.
///
/// It also fixes the weight of an array-valued key. `commandSuggestions`
/// expands `partKey` / `aliasKey` / `descriptionKey` into one entry per element
/// (`:41-49`), and every element shares that key's single normalized weight —
/// so a command with a long description does not dilute its own `commandName`.
#[derive(Clone, Copy, Debug)]
pub struct KeyStore {
    total_weight: f64,
}

impl KeyStore {
    /// `weights` is the configured `keys` table, one entry per key — not per
    /// array element and not per item.
    pub fn new(weights: &[f64]) -> Self {
        let total_weight = weights.iter().sum::<f64>();
        Self {
            total_weight: if total_weight > 0.0 {
                total_weight
            } else {
                1.0
            },
        }
    }

    fn normalize(&self, weight: f64) -> f64 {
        weight / self.total_weight
    }
}

/// Maps to the Fuse options the call sites set (`FuzzyOptions` /
/// `AdvancedOptions`, `fuse.js:400-426`).
#[derive(Clone, Copy, Debug)]
pub struct FuseOptions {
    /// Fuse `threshold` (default 0.6): where the match algorithm gives up.
    pub threshold: f64,
    /// Fuse `location` (default 0): roughly where the pattern is expected.
    pub location: usize,
    /// Fuse `distance` (default 100): how far from `location` a match may sit
    /// before it scores as a complete mismatch. `commandSuggestions.ts:57`
    /// spells this out at 100 so a hit inside a description can still land.
    pub distance: f64,
    /// Fuse `ignoreLocation` (default false). When true, `location` and
    /// `distance` drop out of the score entirely (`computeScore$1:676-678`).
    pub ignore_location: bool,
    /// Fuse `keys`, already summed — see [`KeyStore`].
    pub keys: KeyStore,
}

impl Default for FuseOptions {
    /// Fuse's own defaults, with a single unweighted key.
    fn default() -> Self {
        Self {
            threshold: 0.6,
            location: 0,
            distance: 100.0,
            ignore_location: false,
            keys: KeyStore::new(&[1.0]),
        }
    }
}

/// Maps to one entry of Fuse's `keys` option, `{ name, weight }`.
///
/// Fuse resolves `name` against the item to read the text out; every call site
/// here already holds that text, so `value` stands where the resolved `name`
/// would be. `weight` is the **raw configured** weight — [`KeyStore`] does the
/// dividing.
#[derive(Clone, Copy, Debug)]
pub struct FuseKey<'a> {
    pub value: &'a str,
    pub weight: f64,
}

impl<'a> FuseKey<'a> {
    pub fn new(value: &'a str, weight: f64) -> Self {
        Self { value, weight }
    }
}

/// Maps to Fuse's field-length norm (`norm()`, `fuse.js:436-452`):
/// `1 / numTokens^(0.5 * fieldNormWeight)` with the default
/// `fieldNormWeight: 1`, rounded to three decimals as Fuse does to keep its
/// index small.
///
/// "The shorter the field, the higher the weight" — the same hit counts for
/// more in a two-word name than buried in a sentence.
fn field_norm(value: &str) -> f64 {
    // Fuse splits on `/[^ ]+/g`, i.e. runs of non-space, so tabs and newlines
    // are not separators here.
    let token_count = value.split(' ').filter(|token| !token.is_empty()).count();
    let token_count = token_count.max(1) as f64;
    let norm = 1.0 / token_count.powf(0.5);
    (norm * 1000.0).round() / 1000.0
}

/// Maps to Fuse's `BitapSearch` (`fuse.js:894`): a pattern compiled once, then
/// tested against many texts.
pub struct BitapSearch {
    pattern_text: String,
    pattern: Pattern,
    /// The pattern scored against itself — the denominator that turns nucleo's
    /// unbounded higher-is-better score into Fuse's bounded 0..1 accuracy.
    perfect_score: u32,
    matcher: Matcher,
    buffer: Vec<char>,
    options: FuseOptions,
}

impl BitapSearch {
    /// Maps to `new BitapSearch(pattern, options)` (`fuse.js:895`).
    pub fn new(pattern: &str, options: FuseOptions) -> Self {
        let pattern_text = pattern.to_lowercase();
        let compiled = Pattern::new(
            &pattern_text,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
        );
        let mut config = Config::DEFAULT;
        // The fuzzy branch cannot use `proximity / distance` (no match position
        // from nucleo's `score`), so the location bias rides here instead.
        config.prefer_prefix = !options.ignore_location;
        let mut matcher = Matcher::new(config);
        let mut buffer = Vec::new();
        let perfect_score = compiled
            .score(Utf32Str::new(&pattern_text, &mut buffer), &mut matcher)
            .unwrap_or(1)
            .max(1);
        Self {
            pattern_text,
            pattern: compiled,
            perfect_score,
            matcher,
            buffer,
            options,
        }
    }

    /// Maps to `computeScore$1(pattern, { errors, currentLocation, ... })`
    /// (`fuse.js:663-685`):
    ///
    /// ```js
    /// const accuracy = errors / pattern.length
    /// if (ignoreLocation) return accuracy
    /// const proximity = Math.abs(expectedLocation - currentLocation)
    /// if (!distance) return proximity ? 1.0 : accuracy
    /// return accuracy + proximity / distance
    /// ```
    fn bitap_score(&self, accuracy: f64, current_location: usize) -> f64 {
        if self.options.ignore_location {
            return accuracy;
        }
        let proximity = (self.options.location as f64 - current_location as f64).abs();
        if self.options.distance == 0.0 {
            return if proximity > 0.0 { 1.0 } else { accuracy };
        }
        accuracy + proximity / self.options.distance
    }

    /// Maps to `BitapSearch.searchIn(text)` (`fuse.js:961`), which returns
    /// `{ isMatch, score }`. `None` here is `isMatch: false` — either no match,
    /// or one worse than `threshold`.
    ///
    /// Note what an unconditional zero requires: Fuse's exact-match short
    /// circuit is `this.pattern === text` (:969), the **whole field**. A
    /// substring is not that case — it is bitap with `errors: 0`, so its score
    /// collapses to the location penalty alone. Scoring every substring 0 would
    /// contradict `location: 0` ("prefer matches at the beginning of strings",
    /// `commandSuggestions.ts:56`) and make `distance` unobservable.
    pub fn search_in(&mut self, text: &str) -> Option<f64> {
        if self.pattern_text.is_empty() {
            return Some(0.0);
        }
        let text = text.to_lowercase();
        if text == self.pattern_text {
            return Some(0.0);
        }

        if let Some(position) = text.find(&self.pattern_text) {
            // Byte offset is a character offset here only for ASCII; Fuse
            // counts UTF-16 units, so measure the prefix in chars.
            let current_location = text[..position].chars().count();
            let score = self.bitap_score(0.0, current_location);
            return (score <= self.options.threshold).then_some(score);
        }

        // Approximate match: nucleo stands in for bitap. `accuracy` is the
        // 0..1 projection of nucleo's score; the location term is already
        // folded into that score via `prefer_prefix` (see `new`).
        let score = self
            .pattern
            .score(Utf32Str::new(&text, &mut self.buffer), &mut self.matcher)?;
        let accuracy = (1.0 - f64::from(score) / f64::from(self.perfect_score)).clamp(0.0, 1.0);
        (accuracy <= self.options.threshold).then_some(accuracy)
    }

    /// Maps to `computeScore(results, ...)` (`fuse.js:1670`) applied to one
    /// item's keys:
    ///
    /// ```js
    /// totalScore *= Math.pow(
    ///   score === 0 && weight ? Number.EPSILON : score,
    ///   (weight || 1) * (ignoreFieldNorm ? 1 : norm)
    /// )
    /// ```
    ///
    /// Three details carry the behaviour, and getting any of them wrong
    /// inverts the ranking:
    ///
    /// - the weight is an **exponent**, not a linear scale. Scaling linearly
    ///   caps a light key's best possible score at `1 - w/max`, so a literal
    ///   hit on a description would lose to a merely fuzzy hit on a heavier
    ///   key. As an exponent the weight sharpens the good end without imposing
    ///   a floor.
    /// - a perfect 0 becomes `EPSILON` first. Raising a literal 0 to any power
    ///   is still 0, which would make every perfect hit indistinguishable;
    ///   `EPSILON^w` keeps them ordered by weight while staying far below any
    ///   inexact match.
    /// - only matched keys are multiplied, mirroring Fuse iterating
    ///   `result.matches`. Matching more keys is therefore strictly better.
    ///
    /// The weights come from [`KeyStore`], normalized against the configured
    /// table — not against the keys passed in here.
    pub fn compute_score(&mut self, keys: &[FuseKey<'_>]) -> Option<f64> {
        let mut total_score = 1.0_f64;
        let mut matched = false;
        for key in keys {
            let Some(score) = self.search_in(key.value) else {
                continue;
            };
            matched = true;
            let base = if score == 0.0 { f64::EPSILON } else { score };
            let weight = self.options.keys.normalize(key.weight);
            total_score *= base.powf(weight * field_norm(key.value));
        }
        matched.then_some(total_score)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(threshold: f64, ignore_location: bool) -> FuseOptions {
        FuseOptions {
            threshold,
            ignore_location,
            ..FuseOptions::default()
        }
    }

    /// Fuse's only unconditional zero is `pattern === text` (:969). A
    /// substring is bitap with `errors: 0`, so it keeps the location penalty.
    #[test]
    fn only_a_whole_field_match_scores_zero() {
        let mut search = BitapSearch::new("alpha", FuseOptions::default());
        assert_eq!(search.search_in("alpha"), Some(0.0), "the whole field");
        // "alpha" sits at offset 3, `distance` is 100.
        assert_eq!(
            search.search_in("an alpha thing"),
            Some(0.03),
            "a substring pays proximity / distance"
        );
        assert_eq!(
            search.search_in("alpha thing"),
            Some(0.0),
            "a substring at offset 0 pays nothing"
        );
        assert_eq!(search.search_in("zzz"), None, "no match at all is dropped");
    }

    /// `distance` is the divisor on that penalty, which is why
    /// `commandSuggestions.ts:57` widens it so descriptions can still match.
    #[test]
    fn distance_scales_the_location_penalty() {
        let late = format!("{}alpha", "x".repeat(20));
        let near = BitapSearch::new(
            "alpha",
            FuseOptions {
                distance: 10.0,
                ..FuseOptions::default()
            },
        )
        .search_in(&late);
        let far = BitapSearch::new(
            "alpha",
            FuseOptions {
                distance: 1000.0,
                ..FuseOptions::default()
            },
        )
        .search_in(&late);
        assert_eq!(near, None, "a tight window rejects a hit 20 chars in");
        assert_eq!(far, Some(0.02), "a wide window barely notices");
    }

    /// The fixture the previous test was missing: a late hit survives with
    /// `ignoreLocation: true` and is rejected without it. This is exactly the
    /// difference between `LogSelector.tsx:304` and the other two sites.
    #[test]
    fn ignore_location_decides_whether_a_late_hit_survives() {
        // Offset 40 with `distance: 100` scores 0.4, past a 0.3 threshold.
        let late = format!("{}alpha", "x".repeat(40));
        assert_eq!(
            BitapSearch::new("alpha", options(0.3, false)).search_in(&late),
            None,
            "the location penalty pushes it past the threshold"
        );
        assert_eq!(
            BitapSearch::new("alpha", options(0.3, true)).search_in(&late),
            Some(0.0),
            "ignoring location leaves a clean substring hit"
        );
    }

    /// An earlier hit beats a later one — the whole point of `location: 0`.
    #[test]
    fn an_earlier_substring_beats_a_later_one() {
        let mut search = BitapSearch::new("alpha", FuseOptions::default());
        let early = search.search_in("alpha tail").expect("early hit");
        let late = search
            .search_in("a longer prefix then alpha")
            .expect("late hit");
        assert!(early < late, "earlier wins: {early} vs {late}");
    }

    /// The threshold is a real cutoff, not decoration.
    #[test]
    fn a_stricter_threshold_rejects_more() {
        let loose = BitapSearch::new("alpha", FuseOptions::default()).search_in("a-l-p-h-a");
        let strict = BitapSearch::new("alpha", options(0.0, false)).search_in("a-l-p-h-a");
        assert!(loose.is_some(), "the default threshold keeps it");
        assert_eq!(strict, None, "a zero threshold keeps only perfect hits");
    }

    /// `KeyStore` divides by the CONFIGURED table, so a key keeps its weight
    /// whatever subset of keys an item populates. The two calls below stand in
    /// for an MCP row (four keys) and an agent row (three) under
    /// `unifiedSuggestions`'s five-key, total-10 configuration.
    #[test]
    fn key_weights_normalize_against_the_configured_table_not_the_item() {
        let configured = FuseOptions {
            keys: KeyStore::new(&[2.0, 3.0, 1.0, 1.0, 3.0]),
            ..FuseOptions::default()
        };
        let mut search = BitapSearch::new("alpha", configured);
        let four_keys = search
            .compute_score(&[
                FuseKey::new("nothing", 2.0),
                FuseKey::new("alpha", 3.0),
                FuseKey::new("nothing", 1.0),
                FuseKey::new("nothing", 1.0),
            ])
            .expect("the weight-3 key matches");
        let three_keys = search
            .compute_score(&[
                FuseKey::new("nothing", 2.0),
                FuseKey::new("nothing", 1.0),
                FuseKey::new("alpha", 3.0),
            ])
            .expect("the weight-3 key matches");
        assert_eq!(
            four_keys, three_keys,
            "the same key scores the same however many siblings it has"
        );

        // Re-normalizing per item would have produced 3/7 and 3/6 — different
        // numbers for the same key.
        let per_item = KeyStore::new(&[2.0, 3.0, 1.0, 1.0]);
        assert!(
            (per_item.normalize(3.0) - configured.keys.normalize(3.0)).abs() > f64::EPSILON,
            "the two normalizations really do differ, so the test above has teeth"
        );
    }

    /// An array-valued key expands into one entry per element, and every
    /// element shares the key's single normalized weight — so a long
    /// description cannot dilute `commandName`.
    #[test]
    fn array_keys_do_not_dilute_their_siblings() {
        // commandSuggestions: commandName 3, partKey 2, aliasKey 2,
        // descriptionKey 0.5 — total 7.5 regardless of element counts.
        let configured = FuseOptions {
            threshold: 0.3,
            keys: KeyStore::new(&[3.0, 2.0, 2.0, 0.5]),
            ..FuseOptions::default()
        };
        let mut search = BitapSearch::new("deploy", configured);
        let terse = search
            .compute_score(&[FuseKey::new("deploy", 3.0)])
            .expect("name matches");
        let mut wordy = vec![FuseKey::new("deploy", 3.0)];
        for word in ["some", "long", "description", "words", "here"] {
            wordy.push(FuseKey::new(word, 0.5));
        }
        let wordy = search.compute_score(&wordy).expect("name still matches");
        assert_eq!(
            terse, wordy,
            "unmatched description words leave the name's weight alone"
        );
    }

    /// The weights are the whole point of `keys`.
    #[test]
    fn key_weights_keep_low_value_fields_behind() {
        let configured = FuseOptions {
            keys: KeyStore::new(&[3.0, 1.0]),
            ..FuseOptions::default()
        };
        let mut search = BitapSearch::new("deploy", configured);
        let on_name = search
            .compute_score(&[FuseKey::new("deploy", 3.0), FuseKey::new("unrelated", 1.0)])
            .expect("name matches");
        let on_description = search
            .compute_score(&[FuseKey::new("unrelated", 3.0), FuseKey::new("deploy", 1.0)])
            .expect("description matches");
        assert!(
            on_name < on_description,
            "a name hit beats a description hit: {on_name} vs {on_description}"
        );
    }

    /// With a linear weight scale a light key's literal hit is capped at
    /// `1 - w/max` and loses to a heavy key's fuzzy hit. Fuse never does that.
    #[test]
    fn a_literal_hit_on_a_light_key_still_beats_a_fuzzy_hit_on_a_heavy_one() {
        let configured = FuseOptions {
            keys: KeyStore::new(&[3.0, 1.0]),
            ..FuseOptions::default()
        };
        let mut search = BitapSearch::new("alpha", configured);
        let literal_on_light = search
            .compute_score(&[
                FuseKey::new("nothing", 3.0),
                FuseKey::new("alpha here", 1.0),
            ])
            .expect("light key matches literally");
        let fuzzy_on_heavy = search
            .compute_score(&[FuseKey::new("a-l-p-h-a", 3.0), FuseKey::new("nothing", 1.0)])
            .expect("heavy key matches fuzzily");
        assert!(
            literal_on_light < fuzzy_on_heavy,
            "literal beats fuzzy regardless of weight: {literal_on_light} vs {fuzzy_on_heavy}"
        );
    }

    /// Fuse multiplies over every matched key, so matching more is strictly
    /// better — and matching none drops the row.
    #[test]
    fn matching_more_keys_scores_better_than_matching_one() {
        let configured = FuseOptions {
            keys: KeyStore::new(&[1.0, 1.0]),
            ..FuseOptions::default()
        };
        let mut search = BitapSearch::new("alpha", configured);
        let one_key = search
            .compute_score(&[
                FuseKey::new("nothing here", 1.0),
                FuseKey::new("alpha", 1.0),
            ])
            .expect("one key matches");
        let both_keys = search
            .compute_score(&[FuseKey::new("alpha", 1.0), FuseKey::new("alpha", 1.0)])
            .expect("both keys match");
        assert!(
            both_keys < one_key,
            "two matched keys beat one: {both_keys} vs {one_key}"
        );
        assert_eq!(
            search.compute_score(&[FuseKey::new("nothing", 1.0), FuseKey::new("here", 1.0)]),
            None,
            "no key matching drops the row"
        );
    }

    /// `norm()` is `1/sqrt(tokenCount)` rounded to three decimals.
    #[test]
    fn field_norm_matches_the_source_formula() {
        assert_eq!(field_norm("one"), 1.0);
        assert_eq!(field_norm("one two"), 0.707);
        assert_eq!(field_norm("one two three four"), 0.5);
        assert_eq!(
            field_norm(""),
            1.0,
            "an empty field does not divide by zero"
        );

        let mut search = BitapSearch::new("alpha", FuseOptions::default());
        let short = search
            .compute_score(&[FuseKey::new("alpha", 1.0)])
            .expect("short field matches");
        let long = search
            .compute_score(&[FuseKey::new("alpha then several more words", 1.0)])
            .expect("long field matches");
        assert!(
            short < long,
            "the shorter field weighs more: {short} vs {long}"
        );
    }
}
