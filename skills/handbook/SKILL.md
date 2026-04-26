---
name: handbook
description: Use with `git mesh` or meshes.
---

<instructions>
- **Output or `additionalContent` shows mesh ranges with `[CHANGED]`, `[MOVED]`, `FRESH`, `(ack)`, or `src=…` and the markers need interpreting**: Read `./sections/reading-stale-output.md`
- **A mesh range on a file just edited is drifting and a decision is needed (re-anchor, fix partner, update the why, leave it), or resolver config / `mv` / `delete` / `revert` is in play**: Read `./sections/responding-to-drift.md`
- **A new relationship needs a mesh, or a mesh needs a name, why, range shape, or commit sequence**: Read `./sections/creating-a-mesh.md`
- **An advice session needs setting up, baselining, or interpreting; `git mesh advice <id> snapshot|read` or a bare render is in play; or session state seems stale or absent**: Read `./sections/using-advice.md`
- **A finding is `ORPHANED`, `MERGE_CONFLICT`, `SUBMODULE`, or `SidecarTampered`**: Read `./sections/terminal-statuses.md`
- **A finding is `CONTENT_UNAVAILABLE(...)`, or the failure involves LFS, partial clone, or sparse checkout**: Read `./sections/content-unavailable.md`
- **The range omits `#L…`, or the path is binary, image, symlink, submodule root, or LFS-tracked**: Read `./sections/whole-file-and-lfs.md`
- **A `git mesh` command errored or behaved unexpectedly ("nothing staged", "needs a why", staged sidecar drift, `git log --all` noise, `doctor`)**: Read `./sections/command-quirks-and-errors.md`
- **The job is CI wiring, PR gating, `--since <merge-base>`, `fetch`/`push`, fresh-clone tolerance, or advisory reports**: Read `./sections/ci-and-sync.md`
- **A question asks what meshes exist, what a mesh currently says, its history, or which meshes touch a given path/range**: Read `./sections/inspecting-meshes.md`
- **Exact flag, subcommand, range grammar, or reserved-name lookup is needed**: Read `./sections/command-reference.md`
</instructions>
