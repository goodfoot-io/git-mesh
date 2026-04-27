//! Content cohesion stage (Section 11 of analyze-v4.mjs).
//!
//! Computes four granularities of cohesion:
//!   1. Per-edge: IDF-weighted shared identifiers between two ranges.
//!   2. Clique intersection: identifiers in EVERY range, IDF-weighted.
//!   3. Clique pairwise-min/median/mean: per-pair cohesion statistics.
//!   4. Clique trigram: minimum pairwise Jaccard of trigram sets.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::advice::suggest::canonical::CanonicalIndex;

/// Canonical range identifier — index into `CanonicalIndex::ranges`.
pub type CanonicalId = usize;

// ── IDF type ─────────────────────────────────────────────────────────────────

/// IDF map: token → log((N+1)/(1+df)).
pub type Idf = BTreeMap<String, f64>;

// ── Source cache ──────────────────────────────────────────────────────────────

/// Cached token data for a single range (keyed by `"{path}#{start}-{end}"`).
#[derive(Clone, Debug)]
pub struct RangeTokens {
    /// Unique identifiers (length ≥ 3, not a keyword) found in the range.
    pub identifiers: BTreeSet<String>,
    /// Character trigrams of the sorted identifier sequence.
    pub trigrams: BTreeSet<String>,
}

/// Cache mapping `"{path}#{start}-{end}"` to parsed token data.
pub type SourceCache = BTreeMap<String, RangeTokens>;

// ── KEYWORDS set ─────────────────────────────────────────────────────────────

fn is_keyword(s: &str) -> bool {
    matches!(
        s,
        "fn" | "let" | "mut" | "pub" | "use" | "mod" | "self" | "Self" | "super" | "crate"
            | "struct" | "enum" | "impl" | "trait" | "where" | "as" | "in" | "if" | "else"
            | "match" | "for" | "while" | "loop" | "return" | "break" | "continue"
            | "true" | "false" | "None" | "Some" | "Ok" | "Err" | "Result" | "Option"
            | "String" | "str" | "usize" | "isize" | "u8" | "u16" | "u32" | "u64"
            | "i8" | "i16" | "i32" | "i64" | "bool" | "Vec" | "Box" | "Arc" | "Rc"
            | "PathBuf" | "Path" | "HashMap" | "HashSet" | "BTreeMap" | "and" | "or"
            | "not" | "await" | "async" | "dyn" | "ref" | "static" | "const" | "echo"
            | "set" | "done" | "then" | "esac" | "case" | "function" | "var" | "this"
            | "new" | "class" | "export" | "import" | "from" | "default" | "extends"
            | "implements" | "interface" | "type" | "void" | "null" | "undefined"
            | "number" | "string" | "boolean" | "object" | "any" | "http" | "https"
            | "com" | "org" | "www" | "TODO" | "FIXME" | "XXX" | "NOTE" | "the"
            | "with" | "when" | "that" | "into" | "has" | "have" | "are" | "was"
            | "were" | "been" | "being"
    )
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Read lines `[start, end]` (1-based, inclusive) from a file under `repo_root`.
///
/// Returns `None` if the file does not exist or cannot be read.
pub fn read_range(repo_root: &Path, p: &str, start: u32, end: u32) -> Option<String> {
    let fp = repo_root.join(p);
    let text = std::fs::read_to_string(&fp).ok()?;
    let lines: Vec<&str> = text.split('\n').collect();
    let lo = (start as usize).saturating_sub(1);
    let hi = (end as usize).min(lines.len());
    if lo >= hi && lo >= lines.len() {
        return None;
    }
    Some(lines[lo..hi].join("\n"))
}

/// Extract unique non-keyword identifiers (length ≥ 3) from `text`.
///
/// Ports `tokensOf` from `docs/analyze-v4.mjs` line 651.
/// Matches `[A-Za-z_][A-Za-z0-9_]{2,}` — total length ≥ 3.
/// Returns a sorted `Vec<String>` for determinism.
pub fn tokens_of(text: &str) -> Vec<String> {
    let mut out = BTreeSet::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '_')
            {
                i += 1;
            }
            if i - start >= 3 {
                let s: String = chars[start..i].iter().collect();
                if !is_keyword(&s) {
                    out.insert(s);
                }
            }
        } else {
            i += 1;
        }
    }
    out.into_iter().collect()
}

/// Compute character trigrams of the sorted identifier sequence.
///
/// Ports `trigramsOf` from `docs/analyze-v4.mjs` line 660.
pub fn trigrams_of(text: &str) -> BTreeSet<String> {
    let tokens = tokens_of(text);
    // JS: [...tokensOf(text)].sort().join(' ') — tokens_of already sorts.
    let joined = tokens.join(" ");
    let chars: Vec<char> = joined.chars().collect();
    let mut out = BTreeSet::new();
    for i in 0..chars.len().saturating_sub(2) {
        let tri: String = chars[i..i + 3].iter().collect();
        out.insert(tri);
    }
    out
}

/// Jaccard similarity between two trigram / identifier sets.
///
/// Ports `jaccard` from `docs/analyze-v4.mjs` line 668.
pub fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count();
    inter as f64 / (a.len() + b.len() - inter) as f64
}

/// Build an IDF map from a collection of per-range identifier sets.
///
/// Ports `buildIdf` from `docs/analyze-v4.mjs` line 675.
pub fn build_idf(range_tokens: &BTreeMap<CanonicalId, Vec<String>>) -> Idf {
    let mut df: BTreeMap<String, usize> = BTreeMap::new();
    for tokens in range_tokens.values() {
        for t in tokens {
            *df.entry(t.clone()).or_default() += 1;
        }
    }
    let n = range_tokens.len().max(1);
    df.into_iter()
        .map(|(t, c)| {
            let idf = ((n + 1) as f64 / (1 + c) as f64).ln();
            (t, idf)
        })
        .collect()
}

/// Minimum pairwise Jaccard of trigram sets across all pairs in `range_tokens`.
///
/// Ports `trigramCohesion` from `docs/analyze-v4.mjs` line 738.
pub fn trigram_cohesion(range_tokens: &BTreeMap<CanonicalId, BTreeSet<String>>) -> f64 {
    let items: Vec<&BTreeSet<String>> = range_tokens.values().collect();
    if items.len() < 2 {
        return 0.0;
    }
    let mut min = 1.0_f64;
    for i in 0..items.len() {
        for j in (i + 1)..items.len() {
            let j_ = jaccard(items[i], items[j]);
            if j_ < min {
                min = j_;
            }
        }
    }
    min
}

/// Per-edge cohesion: IDF-weighted shared identifiers between two ranges.
///
/// Ports `pairCohesion` from `docs/analyze-v4.mjs` line 685. Fills the
/// `Option<f64>` seam left by `score_edges`.
///
/// Returns the weight in [0, 1] clamped at `shared_id_saturation`.
pub fn per_edge_cohesion(
    tokens_a: &RangeTokens,
    tokens_b: &RangeTokens,
    idf: &Idf,
    shared_id_saturation: u32,
) -> f64 {
    // Only identifiers of length >= 4, shared by both ranges.
    let inter: Vec<f64> = tokens_a
        .identifiers
        .intersection(&tokens_b.identifiers)
        .filter(|t| t.len() >= 4)
        .map(|t| *idf.get(t).unwrap_or(&0.0))
        .collect();
    let weight_sum: f64 = inter.iter().sum();
    (weight_sum / shared_id_saturation as f64).min(1.0)
}

/// Build a `RangeTokens` from raw text (used to populate a `SourceCache`).
pub fn range_tokens_of(text: &str) -> RangeTokens {
    let tokens = tokens_of(text);
    let identifiers: BTreeSet<String> = tokens.into_iter().collect();
    let trigrams = {
        // Recompute trigrams from the identifier set (sorted, joined).
        let joined = {
            let mut v: Vec<&str> = identifiers.iter().map(|s| s.as_str()).collect();
            v.sort();
            v.join(" ")
        };
        let chars: Vec<char> = joined.chars().collect();
        let mut out = BTreeSet::new();
        for i in 0..chars.len().saturating_sub(2) {
            let tri: String = chars[i..i + 3].iter().collect();
            out.insert(tri);
        }
        out
    };
    RangeTokens {
        identifiers,
        trigrams,
    }
}

/// Populate a `SourceCache` entry for a canonical range, reading from disk.
///
/// Returns `None` if the file cannot be read.
pub fn cache_range(
    repo_root: &Path,
    path: &str,
    start: u32,
    end: u32,
    cache: &mut SourceCache,
) -> Option<()> {
    let key = format!("{path}#{start}-{end}");
    if cache.contains_key(&key) {
        return Some(());
    }
    let text = read_range(repo_root, path, start, end)?;
    cache.insert(key, range_tokens_of(&text));
    Some(())
}

/// Intersection cohesion: identifiers in EVERY range, IDF-weighted.
///
/// Ports `intersectionCohesion` from `docs/analyze-v4.mjs` line 696.
pub fn intersection_cohesion(
    canon_ids: &[CanonicalId],
    cache: &SourceCache,
    canonical: &CanonicalIndex,
    idf: &Idf,
    shared_id_saturation: u32,
) -> (f64, Vec<String>) {
    if canon_ids.is_empty() {
        return (0.0, vec![]);
    }
    let tokens: Vec<Option<&RangeTokens>> = canon_ids
        .iter()
        .map(|&id| {
            let r = canonical.ranges.get(id)?;
            let key = format!("{}#{}-{}", r.path, r.start, r.end);
            cache.get(&key)
        })
        .collect();
    if tokens.iter().any(|t| t.is_none()) {
        return (0.0, vec![]);
    }
    let items: Vec<&RangeTokens> = tokens.into_iter().map(|t| t.unwrap()).collect();
    // Intersection of identifier sets across all ranges.
    let mut inter: BTreeSet<&str> = items[0].identifiers.iter().map(|s| s.as_str()).collect();
    for item in &items[1..] {
        inter.retain(|t| item.identifiers.contains(*t));
    }
    // Filter ≥4 chars, rank by IDF descending.
    let mut ranked: Vec<(&str, f64)> = inter
        .into_iter()
        .filter(|t| t.len() >= 4)
        .map(|t| (t, *idf.get(t).unwrap_or(&0.0)))
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let weight = (ranked.iter().map(|(_, v)| v).sum::<f64>() / shared_id_saturation as f64)
        .min(1.0);
    let display: Vec<String> = ranked
        .iter()
        .take(8)
        .map(|(t, _)| t.to_string())
        .collect();
    (weight, display)
}

/// Pairwise cohesion statistics: min, median, mean over all pairs in the clique.
///
/// Ports `pairwiseCohesionStats` from `docs/analyze-v4.mjs` line 714.
pub struct PairwiseCohesionStats {
    pub min: f64,
    pub median: f64,
    pub mean: f64,
    /// The weakest pair (canonical ids), if any.
    pub weakest_pair: Option<[CanonicalId; 2]>,
}

pub fn pairwise_cohesion_stats(
    canon_ids: &[CanonicalId],
    cache: &SourceCache,
    canonical: &CanonicalIndex,
    idf: &Idf,
    shared_id_saturation: u32,
) -> PairwiseCohesionStats {
    if canon_ids.len() < 2 {
        return PairwiseCohesionStats {
            min: 0.0,
            median: 0.0,
            mean: 0.0,
            weakest_pair: None,
        };
    }
    let mut weights: Vec<f64> = Vec::new();
    let mut min_w = f64::INFINITY;
    let mut weakest_pair: Option<[CanonicalId; 2]> = None;

    for i in 0..canon_ids.len() {
        for j in (i + 1)..canon_ids.len() {
            let a = canon_ids[i];
            let b = canon_ids[j];
            let w = get_pair_weight(a, b, cache, canonical, idf, shared_id_saturation);
            weights.push(w);
            if w < min_w {
                min_w = w;
                weakest_pair = Some([a, b]);
            }
        }
    }
    weights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let min = weights[0];
    let median = weights[weights.len() / 2];
    let mean = weights.iter().sum::<f64>() / weights.len() as f64;
    PairwiseCohesionStats {
        min,
        median,
        mean,
        weakest_pair,
    }
}

fn get_pair_weight(
    a: CanonicalId,
    b: CanonicalId,
    cache: &SourceCache,
    canonical: &CanonicalIndex,
    idf: &Idf,
    shared_id_saturation: u32,
) -> f64 {
    let ra = match canonical.ranges.get(a) {
        Some(r) => r,
        None => return 0.0,
    };
    let rb = match canonical.ranges.get(b) {
        Some(r) => r,
        None => return 0.0,
    };
    let key_a = format!("{}#{}-{}", ra.path, ra.start, ra.end);
    let key_b = format!("{}#{}-{}", rb.path, rb.start, rb.end);
    match (cache.get(&key_a), cache.get(&key_b)) {
        (Some(ta), Some(tb)) => per_edge_cohesion(ta, tb, idf, shared_id_saturation),
        _ => 0.0,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_of_filters_keywords_and_short_tokens() {
        let text = "fn myFunction let x = doSomething(self)";
        let toks = tokens_of(text);
        assert!(!toks.iter().any(|t| t == "fn" || t == "let" || t == "self"));
        assert!(toks.contains(&"myFunction".to_string()));
        assert!(toks.contains(&"doSomething".to_string()));
        // "x" is too short
        assert!(!toks.contains(&"x".to_string()));
    }

    #[test]
    fn trigrams_of_produces_character_trigrams() {
        let text = "abc def";
        let tg = trigrams_of(text);
        // tokens = ["abc", "def"] sorted and joined → "abc def"
        // trigrams: "abc", "bc ", "c d", " de", "def"
        assert!(tg.contains("abc"));
        assert!(tg.contains("def"));
    }

    #[test]
    fn jaccard_empty_returns_zero() {
        let a: BTreeSet<String> = BTreeSet::new();
        let b: BTreeSet<String> = ["hello".to_string()].into();
        assert_eq!(jaccard(&a, &b), 0.0);
    }

    #[test]
    fn jaccard_identical_returns_one() {
        let a: BTreeSet<String> = ["foo".to_string(), "bar".to_string()].into();
        let b = a.clone();
        assert!((jaccard(&a, &b) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn jaccard_disjoint_returns_zero() {
        let a: BTreeSet<String> = ["foo".to_string()].into();
        let b: BTreeSet<String> = ["bar".to_string()].into();
        assert_eq!(jaccard(&a, &b), 0.0);
    }

    #[test]
    fn build_idf_assigns_higher_weight_to_rare_tokens() {
        let mut map: BTreeMap<CanonicalId, Vec<String>> = BTreeMap::new();
        map.insert(0, vec!["rare".to_string(), "common".to_string()]);
        map.insert(1, vec!["common".to_string()]);
        let idf = build_idf(&map);
        // "rare" appears in 1 doc, "common" in 2 docs.
        assert!(idf["rare"] > idf["common"]);
    }

    #[test]
    fn trigram_cohesion_below_two_ranges_returns_zero() {
        let mut map: BTreeMap<CanonicalId, BTreeSet<String>> = BTreeMap::new();
        map.insert(0, ["abc".to_string()].into());
        assert_eq!(trigram_cohesion(&map), 0.0);
    }

    #[test]
    fn trigram_cohesion_identical_ranges_returns_one() {
        let tg: BTreeSet<String> = ["abc".to_string(), "bcd".to_string()].into();
        let mut map: BTreeMap<CanonicalId, BTreeSet<String>> = BTreeMap::new();
        map.insert(0, tg.clone());
        map.insert(1, tg);
        assert!((trigram_cohesion(&map) - 1.0).abs() < 1e-9);
    }
}
