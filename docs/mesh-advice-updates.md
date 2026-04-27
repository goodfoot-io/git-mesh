# Mesh advice — pending updates

## Honor line ranges on the write side

Today, any modification to a meshed *path* fires that path's partner advisories regardless of which lines were actually edited. Reads are already line-precise; writes are not.

### Practical impact

- Every edit anywhere in a meshed file fires all of that file's partner advisories, even when the edit is far from the meshed region. A mesh covering 5% of a 1000-line file produces ~20× more partner-surfacings than warranted; ~95% of edits to that file land outside the meshed region but still trigger.
- The advisory text gives the reader no way to tell whether the actual edit overlapped the mesh range. They must open the diff to find out.
- Meshing small contracts inside large files becomes self-defeating: the natural escape ("mesh the whole file") is a less precise contract and propagates to larger partner surfacings.
- No false negatives: a write to a meshed file always fires its partners, so silence still means the contract is untouched.
- Drift status itself (`FRESH` / `MOVED` / `CHANGED`) is unaffected — what is wrong is *when* partner advice is surfaced, not whether drift detection is correct.

### Desired outcome

Partner advice should fire only when an edit's line range overlaps the mesh range, matching the precision the read side already provides. A reader of the advisory should be able to trust that the mesh was actually touched, not merely that the file was.

## Mesh recommendations should be n-ary

Today the new-mesh detector emits only unordered pairs: when paths A, B, C all qualify together, the reader sees three separate two-file suggestions (A↔B, A↔C, B↔C) rather than one A/B/C recommendation. A mesh in `git mesh` is not constrained to two anchors, so the pair-only output is arbitrary — and often incorrect, because a real coupling among three or more files gets shattered into pairwise fragments that each look weaker than the underlying relationship.

### Desired outcome

When the detector identifies a set of files that move together, it should propose a single n-ary mesh over the full set, not a fan-out of pairwise suggestions. The number of anchors in the recommendation should reflect the inferred relationship, with no upper bound imposed by the surfacing layer.

## Recommendations must include line ranges

Every recommendation should anchor each participating file to a specific line range. A recommendation without a line range is a whole-file mesh, which inherits the same coarseness problem described in the line-range section above and is rarely what the user actually wants.

### Desired outcome

If the detector cannot deduce a line range for a file, that file is not included in the recommendation, and a recommendation that would consist only of unranged files is not surfaced at all. The single carve-out is files that inherently do not support line ranges — binary files, submodules, and similar opaque entries — for which a whole-file anchor is the only meaningful unit. In that case the recommendation may include the unranged file, but only because no finer granularity exists, not because we lacked the signal to compute one.

## Use mesh terminology, not "group"

The system's primitive is a *mesh*. Surfacing copy that says "group" forces the reader to translate to the workflow they would actually use (`git mesh add`). Render text and any user-visible reason names should use mesh terminology consistently.
