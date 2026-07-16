//! 0.5.45 naming-addressing (design §3.2/§4.1) — shared pure name
//! similarity ranking helper.
//!
//! **Pure**: no filesystem reads, no state loads, no terminal
//! multiplexer calls, no message dispatch. The RED-6 governance guard
//! scans this file for the forbidden import tokens and rejects any
//! occurrence.
//!
//! **Single source of truth for name-similarity ranking**: consumed by
//!   - `cli/emit.rs::nearest_subcommand` (unknown top-level subcommand hint),
//!   - `cli/named_address.rs` (bare/`--to-name` typo candidates),
//!   - `cli/send.rs` positional adapter (unknown short id in selected team),
//!   - `mcp_server/tools.rs` owner-scoped peer suggestions.
//!
//! **Never a routing authority**: the returned ranking is *advisory
//! only*. Callers must still exit 1 / `isError=true` / refuse the
//! request on their original reason; the ranked payload only
//! populates the additive `requested_name` / `suggested_name` /
//! `candidates` fields that get echoed back to the user.
//!
//! Ranking rules (design §3.2, byte-locked by RED-6):
//!   1. Comparison keys are trimmed and ASCII-lowercased.
//!   2. Candidates whose `match_key` prefixes the request outrank
//!      lower-distance non-prefix candidates.
//!   3. Otherwise sort by Levenshtein distance.
//!   4. Threshold widens with request length: `0..=3 → 1`, `4..=6 →
//!      2`, longer → `3`. Nothing beyond the threshold is returned —
//!      no "best guess for a far name".
//!   5. Sort order: `prefix_miss` (bool), `distance`, `stable_key`.
//!   6. Deduplicate on `stable_key`, then cap at 3 results.
//!   7. Canonical spelling comes from the candidate `name` payload
//!      (not the lowercased comparison key) so case is preserved.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;

/// A candidate to rank. `T` is a caller-owned payload returned
/// verbatim in the ranked output — callers use it to keep whatever
/// side data (team_key, workspace, etc.) they need to reconstruct
/// their advisory JSON envelope.
#[derive(Debug, Clone)]
pub struct Candidate<T> {
    /// Comparison key. Callers pass the raw canonical name; this
    /// function trims + lowercases before comparing.
    pub match_key: String,
    /// Stable tie-break key. Typically the same as `match_key`, but
    /// callers with cross-team or cross-workspace candidates use a
    /// composite (`team/agent`) so ordering is deterministic across
    /// otherwise-tied entries.
    pub stable_key: String,
    /// Caller-owned payload returned verbatim.
    pub payload: T,
}

/// Threshold for accepting a candidate given the request length.
/// Byte-locked by RED-6: short requests get 1, medium 2, longer 3.
fn threshold_for_len(len: usize) -> usize {
    match len {
        0..=3 => 1,
        4..=6 => 2,
        _ => 3,
    }
}

/// Rank `candidates` against `request`. Returns up to 3 payloads,
/// most-similar-first. `<request>` casing does not affect matching,
/// only comparison; the caller-supplied payload (which carries the
/// canonical `name`) is what surfaces to the user.
///
/// Empty return means "no acceptable candidate" — do NOT re-rank or
/// synthesize a best-guess elsewhere.
pub fn rank<T: Clone>(request: &str, candidates: &[Candidate<T>]) -> Vec<T> {
    let request_key = normalize(request);
    if request_key.is_empty() {
        return Vec::new();
    }
    let threshold = threshold_for_len(request_key.chars().count());
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut scored: Vec<(bool, usize, String, T)> = Vec::new();
    for candidate in candidates {
        if !seen.insert(candidate.stable_key.clone()) {
            continue;
        }
        let match_key = normalize(&candidate.match_key);
        if match_key.is_empty() {
            continue;
        }
        let distance = levenshtein(&request_key, &match_key);
        // Prefix hit means "request is a prefix of candidate" OR
        // "candidate is a prefix of request" — either direction
        // beats a same-distance edit anywhere in the middle.
        let is_prefix = match_key.starts_with(&request_key) || request_key.starts_with(&match_key);
        // Prefix candidates skip the distance threshold (a 1-char
        // prefix miss with distance 5 is still a legitimate
        // completion hint); non-prefix candidates must clear the
        // threshold.
        if !is_prefix && distance > threshold {
            continue;
        }
        // Prefix-miss is the primary sort key; sorting bools puts
        // false (prefix hit) before true (prefix miss).
        scored.push((
            !is_prefix,
            distance,
            candidate.stable_key.clone(),
            candidate.payload.clone(),
        ));
    }
    scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    scored.into_iter().take(3).map(|(_, _, _, p)| p).collect()
}

/// Convenience wrapper for the common case where callers only need
/// the single best canonical name (a `String`), not full payloads.
/// Returns `None` when no candidate clears the threshold.
pub fn best_name<'a, S: AsRef<str>>(request: &str, candidates: &'a [S]) -> Option<&'a str> {
    let payloads: Vec<Candidate<&'a str>> = candidates
        .iter()
        .map(|c| {
            let name = c.as_ref();
            Candidate {
                match_key: name.to_string(),
                stable_key: name.to_string(),
                payload: name,
            }
        })
        .collect();
    rank(request, &payloads).into_iter().next()
}

fn normalize(input: &str) -> String {
    input.trim().to_ascii_lowercase()
}

/// Standard Levenshtein edit distance. Pure, no dependencies.
/// Compared strings must already be normalized (trim + lowercase) by
/// the caller (`rank` does this internally).
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(name: &str) -> Candidate<String> {
        Candidate {
            match_key: name.to_string(),
            stable_key: name.to_string(),
            payload: name.to_string(),
        }
    }

    #[test]
    fn levenshtein_basic() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("abc", "abd"), 1);
    }

    #[test]
    fn prefix_hit_beats_lower_distance_non_prefix() {
        // "stat" is a prefix of "status" (distance 2) but "slat" is
        // distance 1 (mid-word substitution). Prefix wins.
        let out = rank("stat", &[c("slat"), c("status")]);
        assert_eq!(out, vec!["status".to_string(), "slat".to_string()]);
    }

    #[test]
    fn distance_orders_non_prefix() {
        let out = rank("btea", &[c("bota"), c("beta")]);
        assert_eq!(out[0], "beta"); // 1 edit
        assert_eq!(out[1], "bota"); // 2 edits
    }

    #[test]
    fn stable_key_decides_tie() {
        // Both "beta" and "bota" are distance 2 from "bata".
        let out = rank("bata", &[c("bota"), c("beta")]);
        assert_eq!(out, vec!["beta".to_string(), "bota".to_string()]);
    }

    #[test]
    fn caps_at_three() {
        let out = rank("aaaa", &[c("aaab"), c("aaac"), c("aaad"), c("aaae")]);
        assert_eq!(out.len(), 3);
        assert_eq!(out, vec!["aaab", "aaac", "aaad"]);
    }

    #[test]
    fn case_preserved_in_payload() {
        // "BTEA" normalized matches "Beta" (distance 1); the
        // canonical payload preserves original casing.
        let out = rank("BTEA", &[c("Beta")]);
        assert_eq!(out, vec!["Beta".to_string()]);
    }

    #[test]
    fn far_request_returns_empty() {
        let out = rank("zzzzzz", &[c("beta")]);
        assert!(out.is_empty());
    }

    #[test]
    fn threshold_widens_with_length() {
        assert_eq!(threshold_for_len(0), 1);
        assert_eq!(threshold_for_len(3), 1);
        assert_eq!(threshold_for_len(4), 2);
        assert_eq!(threshold_for_len(6), 2);
        assert_eq!(threshold_for_len(7), 3);
        assert_eq!(threshold_for_len(20), 3);
    }

    #[test]
    fn empty_request_returns_empty() {
        let out = rank("", &[c("beta")]);
        assert!(out.is_empty());
    }

    #[test]
    fn dedup_on_stable_key() {
        let out = rank(
            "beta",
            &[
                c("beta"),
                Candidate {
                    match_key: "beta".to_string(),
                    stable_key: "beta".to_string(),
                    payload: "beta-dup".to_string(),
                },
            ],
        );
        assert_eq!(out, vec!["beta".to_string()]);
    }

    #[test]
    fn best_name_convenience() {
        assert_eq!(best_name("btea", &["beta", "bota"]), Some("beta"));
        assert_eq!(best_name("zzzzzz", &["beta"]), None);
    }

    #[test]
    fn statu_still_maps_to_status() {
        // Byte-lock the existing emit.rs behavior: `statu` → `status`
        // with the shared helper.
        let out = rank(
            "statu",
            &[c("status"), c("send"), c("start"), c("restart")],
        );
        assert_eq!(out[0], "status");
    }
}
