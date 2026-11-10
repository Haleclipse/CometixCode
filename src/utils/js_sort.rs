//! JS `Array.prototype.sort` behavior class for ported comparators.
//!
//! Maps to: ECMA-262 `Array.prototype.sort` (ES2023 §23.1.3.30) as shipped by
//! JSC — the engine Claude Code actually runs on via Bun — and V8. Not a port
//! of any CC file: it replaces the *platform primitive* the source's
//! comparators were written against, the same seam as
//! `grep_tool::javascript_locale_compare` (Intl localeCompare).
//!
//! Why it exists: several comparators ported verbatim from the source are not
//! total orders — `commandSuggestions.ts:466-472` (`|scoreDiff| > 0.1` gate),
//! `LogSelector.tsx:439-447` (60s date-tie window),
//! `ManageMarketplaces.tsx:128-132` (first-match builtin pinning), and the
//! NaN-producing install-count comparators. The spec says an inconsistent
//! comparator yields an implementation-defined order; no engine ever throws.
//! Rust's `slice::sort_by` gained a validity check in 1.81 (driftsort/ipnsort,
//! `sort/shared/smallsort.rs`) and panics with "user-provided comparison
//! function does not correctly implement a total order" — under the release
//! profile's `panic = "abort"` that is a SIGABRT. Measured on the verbatim
//! command comparator: 154/200 random 64-element inputs panic; bun/JSC 1.4 and
//! node/V8 24 run the same inputs with zero exceptions (mirror PR #7 carries
//! the field core dumps).
//!
//! Rules for call sites:
//! * A comparator ported from the source that is NOT a proven total order
//!   sorts through [`sort_by`], with the comparator kept verbatim.
//! * Do NOT "fix" such a comparator into a total order (banding, bucketing):
//!   that changes rankings the source never changed.
//! * Comparators that ARE total orders (locale compare, integer keys,
//!   `total_cmp` chains) stay on `slice::sort_by` — under a total order both
//!   algorithms produce bit-identical output (stable sorts are unique), so
//!   there is nothing to route.

use std::cmp::Ordering;

/// Stable bottom-up merge sort that never validates the comparator.
///
/// The comparator result only picks which side of a merge advances and the
/// indices always advance, so ANY comparator — cyclic, NaN-driven,
/// asymmetric — yields some permutation without panicking, which is the
/// engine behavior class. Under a total order the output is bit-identical to
/// `slice::sort_by` (stable-sort uniqueness; asserted below). Under an
/// inconsistent comparator the exact permutation is implementation-defined by
/// spec, so matching a specific engine bit-for-bit is not a meaningful target;
/// "stable, total-order-correct, never panics" is the maximum fidelity
/// available.
///
/// O(n log n) comparisons, O(n) auxiliary memory. `T: Clone` because merging
/// copies elements through two scratch buffers; every current call site sorts
/// small clone-cheap UI rows.
pub fn sort_by<T: Clone, F: FnMut(&T, &T) -> Ordering>(v: &mut [T], mut compare: F) {
    let len = v.len();
    if len < 2 {
        return;
    }
    let mut src: Vec<T> = v.to_vec();
    let mut dst: Vec<T> = v.to_vec();
    let mut width = 1usize;
    while width < len {
        let mut start = 0usize;
        while start < len {
            let mid = usize::min(start + width, len);
            let end = usize::min(start + 2 * width, len);
            let (mut i, mut j, mut k) = (start, mid, start);
            while i < mid && j < end {
                // `!= Greater` lets equal elements take the left side: stable.
                if compare(&src[i], &src[j]) != Ordering::Greater {
                    dst[k] = src[i].clone();
                    i += 1;
                } else {
                    dst[k] = src[j].clone();
                    j += 1;
                }
                k += 1;
            }
            while i < mid {
                dst[k] = src[i].clone();
                i += 1;
                k += 1;
            }
            while j < end {
                dst[k] = src[j].clone();
                j += 1;
                k += 1;
            }
            start = end;
        }
        std::mem::swap(&mut src, &mut dst);
        width *= 2;
    }
    v.clone_from_slice(&src);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// xorshift32 — deterministic inputs without `rand`, and bit-identical to
    /// the JS-side generator used when this behavior was cross-checked against
    /// JSC and V8.
    struct XorShift32(u32);
    impl XorShift32 {
        fn next(&mut self) -> u32 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            self.0 = x;
            x
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Row {
        score: f64,
        usage: f64,
        idx: usize,
    }

    fn rows(seed: u32, n: usize) -> Vec<Row> {
        let mut rng = XorShift32(seed);
        (0..n)
            .map(|idx| Row {
                score: (rng.next() % 40) as f64 * 0.01,
                usage: (rng.next() % 11) as f64,
                idx,
            })
            .collect()
    }

    /// CC `commandSuggestions.ts:466-472` verbatim — the shape that panics
    /// `slice::sort_by` on 154/200 of these exact inputs.
    fn cc_command_comparator(a: &Row, b: &Row) -> Ordering {
        let score_diff = a.score - b.score;
        if score_diff.abs() > 0.1 {
            return a.score.partial_cmp(&b.score).unwrap_or(Ordering::Equal);
        }
        b.usage.partial_cmp(&a.usage).unwrap_or(Ordering::Equal)
    }

    #[test]
    fn inconsistent_official_comparators_sort_without_panicking() {
        // The command-comparator shape over 200 seeds.
        for seed in 1..=200u32 {
            let original = rows(seed, 64);
            let mut data = original.clone();
            sort_by(&mut data, cc_command_comparator);
            let mut before: Vec<usize> = original.iter().map(|r| r.idx).collect();
            let mut after: Vec<usize> = data.iter().map(|r| r.idx).collect();
            before.sort_unstable();
            after.sort_unstable();
            assert_eq!(before, after, "seed {seed}: output is not a permutation");
        }

        // First-match pinning with antisymmetry-breaking duplicate pinned
        // names present.
        let mut names: Vec<String> = (0..48)
            .map(|i| {
                if i % 3 == 0 {
                    "pinned-market".to_string()
                } else {
                    format!("market-{}", (i * 37) % 23)
                }
            })
            .collect();
        sort_by(&mut names, |a, b| {
            if a == "pinned-market" {
                return Ordering::Less;
            }
            if b == "pinned-market" {
                return Ordering::Greater;
            }
            a.cmp(b)
        });
        assert_eq!(names.len(), 48);

        // NaN through `partial_cmp().unwrap_or(Equal)`.
        let mut rng = XorShift32(7);
        let mut numbers: Vec<f64> = (0..64)
            .map(|_| {
                let r = rng.next() % 10;
                if r == 0 { f64::NAN } else { r as f64 }
            })
            .collect();
        sort_by(&mut numbers, |a, b| {
            a.partial_cmp(b).unwrap_or(Ordering::Equal)
        });
        assert_eq!(numbers.len(), 64);
    }

    /// Stable sorts are unique under a total order, so on every input that IS
    /// consistently ordered this function must agree with `slice::sort_by`
    /// element-for-element — original indices included (identical stability).
    /// This is what makes routing a call site through here a no-op for its
    /// normal, non-crashing data.
    #[test]
    fn total_order_output_is_bit_identical_to_std_sort() {
        let total = |a: &Row, b: &Row| {
            a.score
                .total_cmp(&b.score)
                .then_with(|| b.usage.total_cmp(&a.usage))
        };
        for seed in 1..=200u32 {
            let original = rows(seed, 64);
            let mut ours = original.clone();
            let mut std_sorted = original.clone();
            sort_by(&mut ours, total);
            std_sorted.sort_by(total);
            let ours_idx: Vec<usize> = ours.iter().map(|r| r.idx).collect();
            let std_idx: Vec<usize> = std_sorted.iter().map(|r| r.idx).collect();
            assert_eq!(ours_idx, std_idx, "seed {seed}: stable outputs diverge");
        }
    }

    #[test]
    fn empty_and_single_element_slices_are_untouched() {
        let mut empty: Vec<i32> = Vec::new();
        sort_by(&mut empty, |a, b| a.cmp(b));
        assert!(empty.is_empty());

        let mut single = vec![7];
        sort_by(&mut single, |a, b| a.cmp(b));
        assert_eq!(single, [7]);
    }
}
