//! Clique enumeration stage (Section 12 of analyze-v4.mjs).
//!
//! Builds an edge adjacency graph from scored edges, finds connected components,
//! then runs Bron-Kerbosch with pivot to enumerate all maximal cliques of size
//! in [2, max_clique_size].

use std::collections::BTreeMap;

use crate::advice::suggest::cohesion::CanonicalId;
use crate::advice::suggest::edges::Edge;

// ── Public types ──────────────────────────────────────────────────────────────

/// Adjacency map: node → (neighbor → edge index into the original `edges` slice).
///
/// Uses `BTreeMap` for deterministic iteration order.
pub type Adjacency = BTreeMap<CanonicalId, BTreeMap<CanonicalId, usize>>;

/// A reference to an edge by its index in the original edges slice.
pub type EdgeRef = usize;

// ── Public API ────────────────────────────────────────────────────────────────

/// Build an adjacency map from a slice of edges.
///
/// Ports `buildEdgeAdjacency` from `docs/analyze-v4.mjs` line 754.
pub fn build_edge_adjacency(edges: &[Edge]) -> Adjacency {
    let mut adj: Adjacency = BTreeMap::new();
    for (idx, e) in edges.iter().enumerate() {
        adj.entry(e.canonical_a)
            .or_default()
            .insert(e.canonical_b, idx);
        adj.entry(e.canonical_b)
            .or_default()
            .insert(e.canonical_a, idx);
    }
    adj
}

/// Find connected components of the adjacency graph.
///
/// Ports `connectedComponents` from `docs/analyze-v4.mjs` line 765.
/// Components are returned in ascending node-id order (BTreeMap guarantees
/// deterministic enumeration start points).
pub fn connected_components(adj: &Adjacency) -> Vec<Vec<CanonicalId>> {
    let mut visited: BTreeMap<CanonicalId, bool> = BTreeMap::new();
    let mut out: Vec<Vec<CanonicalId>> = Vec::new();
    for &node in adj.keys() {
        if *visited.get(&node).unwrap_or(&false) {
            continue;
        }
        let mut stack = vec![node];
        let mut comp: Vec<CanonicalId> = Vec::new();
        while let Some(x) = stack.pop() {
            if *visited.get(&x).unwrap_or(&false) {
                continue;
            }
            visited.insert(x, true);
            comp.push(x);
            if let Some(neighbors) = adj.get(&x) {
                for &n in neighbors.keys() {
                    if !visited.get(&n).unwrap_or(&false) {
                        stack.push(n);
                    }
                }
            }
        }
        // Sort component for determinism.
        comp.sort_unstable();
        out.push(comp);
    }
    out
}

/// Bron-Kerbosch with pivot: enumerate all maximal cliques of size in [2, max_size].
///
/// Ports `bronKerbosch` from `docs/analyze-v4.mjs` line 783.
/// Deterministic: component vertices and pivot choices are sorted before use.
pub fn bron_kerbosch(
    component: &[CanonicalId],
    adj: &Adjacency,
    max_size: usize,
) -> Vec<Vec<CanonicalId>> {
    let mut result: Vec<Vec<CanonicalId>> = Vec::new();
    // Sort component for deterministic pivot selection.
    let mut component_sorted = component.to_vec();
    component_sorted.sort_unstable();

    let mut r: Vec<CanonicalId> = Vec::new();
    let p: Vec<CanonicalId> = component_sorted;
    let x: Vec<CanonicalId> = Vec::new();
    bk(&mut r, p, x, adj, max_size, &mut result);
    result
}

fn bk(
    r: &mut Vec<CanonicalId>,
    mut p: Vec<CanonicalId>,
    mut x: Vec<CanonicalId>,
    adj: &Adjacency,
    max_size: usize,
    result: &mut Vec<Vec<CanonicalId>>,
) {
    if p.is_empty() && x.is_empty() {
        if r.len() >= 2 && r.len() <= max_size {
            let mut clique = r.clone();
            clique.sort_unstable();
            result.push(clique);
        }
        return;
    }
    // Choose pivot with maximum connections into P.
    let all_candidates: Vec<CanonicalId> = {
        let mut v = p.clone();
        v.extend_from_slice(&x);
        v.sort_unstable();
        v
    };
    let pivot = all_candidates.iter().max_by_key(|&&u| {
        let empty = BTreeMap::new();
        let neighbors = adj.get(&u).unwrap_or(&empty);
        p.iter().filter(|&&v| neighbors.contains_key(&v)).count()
    });
    let pivot_neighbors: Vec<CanonicalId> = match pivot {
        Some(&pv) => {
            let empty = BTreeMap::new();
            adj.get(&pv)
                .unwrap_or(&empty)
                .keys()
                .copied()
                .collect()
        }
        None => Vec::new(),
    };
    // Candidates = P \ N(pivot), in sorted order for determinism.
    let mut candidates: Vec<CanonicalId> = p
        .iter()
        .copied()
        .filter(|v| !pivot_neighbors.contains(v))
        .collect();
    candidates.sort_unstable();

    for v in candidates {
        let empty = BTreeMap::new();
        let n_v: &BTreeMap<CanonicalId, usize> = adj.get(&v).unwrap_or(&empty);
        let p_new: Vec<CanonicalId> = p.iter().copied().filter(|&u| n_v.contains_key(&u)).collect();
        let x_new: Vec<CanonicalId> = x.iter().copied().filter(|&u| n_v.contains_key(&u)).collect();
        r.push(v);
        bk(r, p_new, x_new, adj, max_size, result);
        r.pop();
        p.retain(|&u| u != v);
        x.push(v);
    }
}

/// Return the edge indices (into the original edges slice) for all pairs within `canon_ids`.
///
/// Ports `edgesWithin` from `docs/analyze-v4.mjs` line 810.
pub fn edges_within(canon_ids: &[CanonicalId], adj: &Adjacency) -> Vec<EdgeRef> {
    let mut out = Vec::new();
    for i in 0..canon_ids.len() {
        for j in (i + 1)..canon_ids.len() {
            let a = canon_ids[i];
            let b = canon_ids[j];
            if let Some(neighbors) = adj.get(&a)
                && let Some(&edge_idx) = neighbors.get(&b)
            {
                out.push(edge_idx);
            }
        }
    }
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advice::suggest::edges::Edge;
    use crate::advice::suggest::edges::ComponentScores;

    fn make_edge(a: usize, b: usize) -> Edge {
        Edge {
            canonical_a: a,
            canonical_b: b,
            score: 0.5,
            components: ComponentScores {
                s_recurrence: 0.0,
                s_confidence: 0.0,
                s_lift: 0.0,
                s_distance: 0.0,
                s_edit: 0.0,
                s_kind: 0.0,
                s_history: 0.0,
            },
            per_edge_cohesion: None,
            shared_sessions: 1,
            mean_op_distance: 1.0,
            lift: 1.0,
            confidence: 0.5,
            support: 0.1,
            edit_hits: 1,
            weighted_hits: 1.0,
            kinds: vec![],
            history_pair_commits: 0,
            history_weighted: 0.0,
        }
    }

    #[test]
    fn complete_4_node_graph_yields_one_4_clique() {
        // K4: nodes 0,1,2,3 with all 6 edges.
        let edges: Vec<Edge> = vec![
            make_edge(0, 1), make_edge(0, 2), make_edge(0, 3),
            make_edge(1, 2), make_edge(1, 3), make_edge(2, 3),
        ];
        let adj = build_edge_adjacency(&edges);
        let comps = connected_components(&adj);
        assert_eq!(comps.len(), 1);
        let cliques = bron_kerbosch(&comps[0], &adj, 8);
        // K4 has exactly one maximal clique (the whole graph).
        assert_eq!(cliques.len(), 1);
        assert_eq!(cliques[0].len(), 4);
    }

    #[test]
    fn two_triangles_sharing_one_edge_yields_two_cliques() {
        // {0,1,2} and {1,2,3}: edges 01,02,12,13,23.
        let edges = vec![
            make_edge(0, 1), make_edge(0, 2), make_edge(1, 2),
            make_edge(1, 3), make_edge(2, 3),
        ];
        let adj = build_edge_adjacency(&edges);
        let comps = connected_components(&adj);
        let cliques = bron_kerbosch(&comps[0], &adj, 8);
        // Two maximal 3-cliques.
        assert_eq!(cliques.len(), 2);
        let mut sizes: Vec<usize> = cliques.iter().map(|c| c.len()).collect();
        sizes.sort();
        assert_eq!(sizes, vec![3, 3]);
    }

    #[test]
    fn path_graph_yields_edge_cliques() {
        // Path 0-1-2-3: maximal cliques are {0,1}, {1,2}, {2,3}.
        let edges = vec![make_edge(0, 1), make_edge(1, 2), make_edge(2, 3)];
        let adj = build_edge_adjacency(&edges);
        let comps = connected_components(&adj);
        let cliques = bron_kerbosch(&comps[0], &adj, 8);
        assert_eq!(cliques.len(), 3);
        assert!(cliques.iter().all(|c| c.len() == 2));
    }

    #[test]
    fn clique_larger_than_max_size_not_emitted() {
        // K4 with max_size=3: only 3-cliques (four of them), not the 4-clique.
        let edges: Vec<Edge> = vec![
            make_edge(0, 1), make_edge(0, 2), make_edge(0, 3),
            make_edge(1, 2), make_edge(1, 3), make_edge(2, 3),
        ];
        let adj = build_edge_adjacency(&edges);
        let comps = connected_components(&adj);
        let cliques = bron_kerbosch(&comps[0], &adj, 3);
        // BK returns maximal cliques; with cap=3 the algorithm still finds the 4-clique
        // but does NOT emit it (size > max_size), so it emits nothing (empty base case).
        // Per the JS: `if (R.length >= 2 && R.length <= maxSize) result.push([...R]);`
        // When R has 4 nodes and maxSize=3, it's not pushed.
        assert!(cliques.iter().all(|c| c.len() <= 3));
    }

    #[test]
    fn edges_within_returns_correct_edge_refs() {
        let edges = vec![
            make_edge(0, 1), // idx 0
            make_edge(0, 2), // idx 1
            make_edge(1, 2), // idx 2
        ];
        let adj = build_edge_adjacency(&edges);
        let within = edges_within(&[0, 1, 2], &adj);
        // All 3 edges are within.
        assert_eq!(within.len(), 3);
    }
}
