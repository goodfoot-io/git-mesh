//! Resolver: compute staleness for ranges and meshes (§5).
//!
//! Phase 1 types slice: the resolver's public signatures remain, but the
//! bodies are stubbed with `todo!()` until the engine/renderer slices
//! land. The new `Finding` / `RangeLocation` shapes (see
//! `docs/stale-layers-plan.md`) are incompatible with the pre-slice
//! resolver, and later slices rewrite this module around
//! `resolver::Engine(LayerSet, Scope)`. Keeping bodies as `todo!()` here
//! avoids partial behaviour drift while the types boundary stabilizes.

#![allow(unused_variables, dead_code)]

use crate::types::{EngineOptions, Mesh, MeshResolved, RangeResolved};
use crate::Result;

pub fn resolve_range(
    _repo: &gix::Repository,
    _mesh_name: &str,
    _range_id: &str,
    _options: EngineOptions,
) -> Result<RangeResolved> {
    todo!("resolve_range is rewritten atop resolver::Engine in the engine slice")
}

pub fn resolve_mesh(
    _repo: &gix::Repository,
    _name: &str,
    _options: EngineOptions,
) -> Result<MeshResolved> {
    todo!("resolve_mesh is rewritten atop resolver::Engine in the engine slice")
}

pub fn culprit_commit(
    _repo: &gix::Repository,
    _resolved: &RangeResolved,
) -> Result<Option<String>> {
    todo!("culprit_commit is rewritten atop resolver::Engine in the engine slice")
}

pub fn stale_meshes(
    _repo: &gix::Repository,
    _options: EngineOptions,
) -> Result<Vec<MeshResolved>> {
    todo!("stale_meshes is rewritten atop resolver::Engine in the engine slice")
}

fn _kept(_: &Mesh) {}
