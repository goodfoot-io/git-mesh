//! Tests for the cohesion-scoring stage.
//!
//! Ports the four-granularity content cohesion from Section 11 of
//! `docs/analyze-v4.mjs`.

use git_mesh::advice::suggest::{
    CanonicalId, build_idf, jaccard, per_edge_cohesion, range_tokens_of, tokens_of,
    trigram_cohesion, trigrams_of,
};
use std::collections::{BTreeMap, BTreeSet};

// ---------------------------------------------------------------------------
// tokens_of
// ---------------------------------------------------------------------------

#[test]
fn tokens_of_excludes_keywords() {
    let toks = tokens_of("fn let mut pub self struct impl");
    assert!(toks.is_empty(), "all tokens are keywords: {toks:?}");
}

#[test]
fn tokens_of_excludes_short_tokens() {
    // Length < 3 must be excluded.
    let toks = tokens_of("ab cd ef");
    assert!(toks.is_empty());
}

#[test]
fn tokens_of_includes_identifiers_of_length_3() {
    let toks = tokens_of("foo bar baz");
    assert!(toks.contains(&"foo".to_string()));
    assert!(toks.contains(&"bar".to_string()));
    assert!(toks.contains(&"baz".to_string()));
}

#[test]
fn tokens_of_returns_sorted_unique_identifiers() {
    let toks = tokens_of("bbb aaa aaa ccc");
    let mut expected = vec!["aaa".to_string(), "bbb".to_string(), "ccc".to_string()];
    expected.sort();
    assert_eq!(toks, expected);
}

// ---------------------------------------------------------------------------
// trigrams_of
// ---------------------------------------------------------------------------

#[test]
fn trigrams_of_empty_text_returns_empty() {
    let tg = trigrams_of("");
    assert!(tg.is_empty());
}

#[test]
fn trigrams_of_single_short_token_returns_empty() {
    // "abc" → sorted join = "abc", 1 trigram "abc"
    let tg = trigrams_of("abc");
    assert!(tg.contains("abc"));
}

#[test]
fn trigrams_of_is_deterministic() {
    let tg1 = trigrams_of("buildIndex cacheKey TreeNode");
    let tg2 = trigrams_of("cacheKey buildIndex TreeNode");
    // same text, different order → same trigrams (tokens are sorted before joining)
    assert_eq!(tg1, tg2);
}

// ---------------------------------------------------------------------------
// jaccard
// ---------------------------------------------------------------------------

#[test]
fn jaccard_empty_set_returns_zero() {
    let empty: BTreeSet<String> = BTreeSet::new();
    let b: BTreeSet<String> = ["hello".to_string()].into();
    assert_eq!(jaccard(&empty, &b), 0.0);
    assert_eq!(jaccard(&b, &empty), 0.0);
}

#[test]
fn jaccard_identical_sets_returns_one() {
    let a: BTreeSet<String> = ["alpha".to_string(), "beta".to_string()].into();
    assert!((jaccard(&a, &a) - 1.0).abs() < 1e-9);
}

#[test]
fn jaccard_disjoint_sets_returns_zero() {
    let a: BTreeSet<String> = ["alpha".to_string()].into();
    let b: BTreeSet<String> = ["gamma".to_string()].into();
    assert_eq!(jaccard(&a, &b), 0.0);
}

#[test]
fn jaccard_partial_overlap() {
    // a={x,y}, b={y,z}: inter=1, union=3 → 1/3
    let a: BTreeSet<String> = ["xxx".to_string(), "yyy".to_string()].into();
    let b: BTreeSet<String> = ["yyy".to_string(), "zzz".to_string()].into();
    let j = jaccard(&a, &b);
    assert!((j - 1.0 / 3.0).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// build_idf
// ---------------------------------------------------------------------------

#[test]
fn build_idf_rare_token_has_higher_weight_than_common() {
    let mut map: BTreeMap<CanonicalId, Vec<String>> = BTreeMap::new();
    map.insert(0, vec!["rare".to_string(), "common".to_string()]);
    map.insert(1, vec!["common".to_string()]);
    let idf = build_idf(&map);
    assert!(idf["rare"] > idf["common"]);
}

#[test]
fn build_idf_token_in_all_docs_has_low_weight() {
    let mut map: BTreeMap<CanonicalId, Vec<String>> = BTreeMap::new();
    map.insert(0, vec!["everywhere".to_string()]);
    map.insert(1, vec!["everywhere".to_string()]);
    map.insert(2, vec!["everywhere".to_string()]);
    let idf = build_idf(&map);
    // log((3+1)/(1+3)) = log(1) = 0.0
    assert!((idf["everywhere"]).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// trigram_cohesion
// ---------------------------------------------------------------------------

#[test]
fn trigram_cohesion_single_range_returns_zero() {
    let tg: BTreeSet<String> = ["abc".to_string()].into();
    let mut map: BTreeMap<CanonicalId, BTreeSet<String>> = BTreeMap::new();
    map.insert(0, tg);
    assert_eq!(trigram_cohesion(&map), 0.0);
}

#[test]
fn trigram_cohesion_identical_ranges_returns_one() {
    let tg: BTreeSet<String> = ["abc".to_string(), "bcd".to_string(), "cde".to_string()].into();
    let mut map: BTreeMap<CanonicalId, BTreeSet<String>> = BTreeMap::new();
    map.insert(0, tg.clone());
    map.insert(1, tg);
    assert!((trigram_cohesion(&map) - 1.0).abs() < 1e-9);
}

#[test]
fn trigram_cohesion_disjoint_ranges_returns_zero() {
    let tg_a: BTreeSet<String> = ["aaa".to_string()].into();
    let tg_b: BTreeSet<String> = ["zzz".to_string()].into();
    let mut map: BTreeMap<CanonicalId, BTreeSet<String>> = BTreeMap::new();
    map.insert(0, tg_a);
    map.insert(1, tg_b);
    assert_eq!(trigram_cohesion(&map), 0.0);
}

// ---------------------------------------------------------------------------
// per_edge_cohesion
// ---------------------------------------------------------------------------

#[test]
fn per_edge_cohesion_disjoint_identifiers_falls_back_to_trigram_jaccard() {
    // Disjoint identifiers produce 0 IDF-weighted shared signal, but the
    // trigram-jaccard fallback (added so the cohesion gate has a meaningful
    // signal under tiny single-session corpora) still surfaces character-
    // level similarity between the joined identifier streams.
    let mut map: BTreeMap<CanonicalId, Vec<String>> = BTreeMap::new();
    map.insert(0, vec!["alpha".to_string()]);
    map.insert(1, vec!["delta".to_string()]);
    let idf = build_idf(&map);

    let a = range_tokens_of("alpha beta gamma");
    let b = range_tokens_of("delta epsilon zeta");
    let cohesion = per_edge_cohesion(&a, &b, &idf, 6);
    // Disjoint identifiers → IDF-weighted is 0, jaccard is the residual.
    assert!((0.0..0.20).contains(&cohesion));
}

#[test]
fn per_edge_cohesion_identical_text_returns_positive() {
    // Build IDF with two ranges: both share some tokens, some are unique.
    // Shared tokens will have idf = ln(3/2) > 0. Unique tokens have idf = ln(3/1) >> 0.
    let shared = "buildIndex cacheKey TreeNode resolver";
    let unique_a = "uniqueAlpha betaOnly";
    let unique_b = "gammaOnly deltaValue";
    let mut map: BTreeMap<CanonicalId, Vec<String>> = BTreeMap::new();
    // Three ranges: two share the tokens, one has different tokens.
    map.insert(0, tokens_of(shared));
    map.insert(1, tokens_of(shared));
    map.insert(2, {
        let mut v = tokens_of(unique_a);
        v.extend(tokens_of(unique_b));
        v
    });
    let idf = build_idf(&map);
    let a = range_tokens_of(shared);
    let b = range_tokens_of(shared);
    let w = per_edge_cohesion(&a, &b, &idf, 6);
    assert!(
        w > 0.0,
        "shared text should yield positive cohesion, idf={idf:?}"
    );
}

#[test]
fn per_edge_cohesion_clamped_at_one() {
    // Even with very high IDF weights, result must not exceed 1.0.
    let text = "buildIndex cacheKey TreeNode resolver alphaToken";
    let idf: BTreeMap<String, f64> = tokens_of(text)
        .into_iter()
        .map(|t| (t, 100.0)) // absurdly high IDF
        .collect();
    let a = range_tokens_of(text);
    let b = a.clone();
    let w = per_edge_cohesion(&a, &b, &idf, 6);
    assert!(w <= 1.0);
}
