//! Tests for the clique-enumeration stage.
//!
//! Ports Section 12 of `docs/analyze-v4.mjs`: buildEdgeAdjacency,
//! connectedComponents, bronKerbosch, edgesWithin.

use git_mesh::advice::suggest::{
    ComponentScores, Edge, bron_kerbosch, build_edge_adjacency, connected_components, edges_within,
};

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

// ---------------------------------------------------------------------------
// build_edge_adjacency
// ---------------------------------------------------------------------------

#[test]
fn adjacency_is_symmetric() {
    let edges = vec![make_edge(0, 1)];
    let adj = build_edge_adjacency(&edges);
    assert!(adj[&0].contains_key(&1));
    assert!(adj[&1].contains_key(&0));
}

#[test]
fn adjacency_stores_correct_edge_index() {
    let edges = vec![make_edge(0, 1), make_edge(1, 2)];
    let adj = build_edge_adjacency(&edges);
    assert_eq!(adj[&0][&1], 0);
    assert_eq!(adj[&1][&2], 1);
}

// ---------------------------------------------------------------------------
// connected_components
// ---------------------------------------------------------------------------

#[test]
fn single_edge_is_one_component() {
    let edges = vec![make_edge(0, 1)];
    let adj = build_edge_adjacency(&edges);
    let comps = connected_components(&adj);
    assert_eq!(comps.len(), 1);
    let mut c = comps[0].clone();
    c.sort();
    assert_eq!(c, vec![0, 1]);
}

#[test]
fn two_disjoint_edges_are_two_components() {
    let edges = vec![make_edge(0, 1), make_edge(2, 3)];
    let adj = build_edge_adjacency(&edges);
    let comps = connected_components(&adj);
    assert_eq!(comps.len(), 2);
}

// ---------------------------------------------------------------------------
// bron_kerbosch
// ---------------------------------------------------------------------------

#[test]
fn complete_4_node_graph_yields_one_4_clique() {
    let edges: Vec<Edge> = vec![
        make_edge(0, 1),
        make_edge(0, 2),
        make_edge(0, 3),
        make_edge(1, 2),
        make_edge(1, 3),
        make_edge(2, 3),
    ];
    let adj = build_edge_adjacency(&edges);
    let comps = connected_components(&adj);
    assert_eq!(comps.len(), 1);
    let cliques = bron_kerbosch(&comps[0], &adj, 8);
    assert_eq!(cliques.len(), 1);
    assert_eq!(cliques[0].len(), 4);
}

#[test]
fn two_triangles_sharing_edge_yields_two_cliques() {
    // {0,1,2} and {1,2,3}
    let edges = vec![
        make_edge(0, 1),
        make_edge(0, 2),
        make_edge(1, 2),
        make_edge(1, 3),
        make_edge(2, 3),
    ];
    let adj = build_edge_adjacency(&edges);
    let comps = connected_components(&adj);
    let cliques = bron_kerbosch(&comps[0], &adj, 8);
    assert_eq!(cliques.len(), 2);
    assert!(cliques.iter().all(|c| c.len() == 3));
}

#[test]
fn path_graph_yields_only_pair_cliques() {
    // 0-1-2-3
    let edges = vec![make_edge(0, 1), make_edge(1, 2), make_edge(2, 3)];
    let adj = build_edge_adjacency(&edges);
    let comps = connected_components(&adj);
    let cliques = bron_kerbosch(&comps[0], &adj, 8);
    assert_eq!(cliques.len(), 3);
    assert!(cliques.iter().all(|c| c.len() == 2));
}

#[test]
fn cliques_respect_max_size_cap() {
    // K4 with max_size=3 must not emit the 4-clique.
    let edges: Vec<Edge> = vec![
        make_edge(0, 1),
        make_edge(0, 2),
        make_edge(0, 3),
        make_edge(1, 2),
        make_edge(1, 3),
        make_edge(2, 3),
    ];
    let adj = build_edge_adjacency(&edges);
    let comps = connected_components(&adj);
    let cliques = bron_kerbosch(&comps[0], &adj, 3);
    assert!(cliques.iter().all(|c| c.len() <= 3));
}

#[test]
fn k8_yields_one_8_clique() {
    // K8: 28 edges.
    let edges: Vec<Edge> = (0..8)
        .flat_map(|a| (a + 1..8).map(move |b| make_edge(a, b)))
        .collect();
    let adj = build_edge_adjacency(&edges);
    let comps = connected_components(&adj);
    let cliques = bron_kerbosch(&comps[0], &adj, 8);
    assert_eq!(cliques.len(), 1);
    assert_eq!(cliques[0].len(), 8);
}

// ---------------------------------------------------------------------------
// edges_within
// ---------------------------------------------------------------------------

#[test]
fn edges_within_returns_all_intra_clique_edges() {
    let edges = vec![
        make_edge(0, 1), // idx 0
        make_edge(0, 2), // idx 1
        make_edge(1, 2), // idx 2
        make_edge(3, 4), // idx 3 — not in clique
    ];
    let adj = build_edge_adjacency(&edges);
    let within = edges_within(&[0, 1, 2], &adj);
    let mut within_sorted = within.clone();
    within_sorted.sort();
    assert_eq!(within_sorted, vec![0, 1, 2]);
}

#[test]
fn edges_within_empty_when_no_intra_edges() {
    let edges = vec![make_edge(0, 3), make_edge(1, 4)];
    let adj = build_edge_adjacency(&edges);
    // Nodes 0,1,2 have no edges among themselves.
    let within = edges_within(&[0, 1, 2], &adj);
    assert!(within.is_empty());
}
