# git mesh-recommend — fine-tuning project

A specific plan for a small fine-tuned text-to-text model that, given a `git mesh`
anchor set plus optional highlight phrases, outputs a `{ "name", "why" }` JSON
recommendation. The model is shipped as part of an architecture-specific Rust
binary running on Sonos `tract`, CPU only, under 250 MB packaged.

This document is *prescriptive*. Where two approaches are plausible, one is chosen.
Deviation requires updating this document.

---

## 1. Hard constraints

- Runtime: `tract-onnx` only (no ONNX Runtime, no candle, no torch).
- CPU only.
- Final artifact directory ≤ **250 MB** total.
- Output: exactly `{ "name": "...", "why": "..." }` JSON. Deterministic.
- Greenfield. No fallbacks, no migrations, no backwards-compatibility shims.
  Fail closed on every error path.

## 2. Pinned decisions

These are *not* discussed further in the document. Treat as load-bearing.

| Decision | Value | Rationale |
| --- | --- | --- |
| Base model | `google/t5-efficient-tiny` | 15.58M params, ~62 MB FP32 weights. Mini (31.2M / ~125 MB) blows the 250 MB cap once encoder + decoder + tokenizer are added. Tiny lands enc+dec at ~130 MB before configs. |
| Precision | FP32 | INT8/FP16 are not validated on tract for T5; budget allows FP32. |
| Decoder | Plain greedy decoder, no kv-cache graph | `decoder_with_past_model.onnx` doubles disk and pulls in `If`-gated cache branches; outputs are ≤ 96 tokens so cache savings are immaterial. |
| ONNX opset | 17 | Widely tested in tract; later opsets pull in ops with weaker tract coverage. |
| Export tool | `optimum-cli export onnx --task text2text-generation` | Produces exactly `encoder_model.onnx` + `decoder_model.onnx`. The `-with-past` task variant is forbidden. |
| Graph slim | Optimum's built-in `onnxslim` (`--optimize O1`) | Optimum integrated `onnxslim` in May 2025; no separate `onnxsim` step. |
| Tokenizer | `tokenizer.json` generated via `transformers.convert_slow_tokenizer.SpmConverter` | Loaded in Rust by the `tokenizers` crate. `spiece.model` is *not* shipped to the runtime artifact. |
| Decoding | Greedy, `max_new_tokens=96`, `do_sample=false`, `num_beams=1` | Matches the deployed runtime. Beam is not used anywhere. |
| Wire format | Tier-prefixed flat text (`@P1 highlight`, `@P2 sibling`, `@P3 anchor.head`/`anchor.body`, `@out`) | Open and minimal; new evidence kinds are new `@Pn` lines, no schema migration. |
| `max_source_length` | 1024 | Empirical: covers ~3 small anchors of source code or one ADR. |
| `max_target_length` | 96 | A name + a one-sentence why fits in 96 tokens with margin. |
| Anchor-order strategy | Shuffle each step at training time. At inference, run twice with different orders and require identical `(name, why)`. | Defends against directional leakage. |
| Frontier LLM for labeling | Claude (this repo's `Anthropic SDK` path) | Used offline by paste-into-prompt; an optional API path may be added later but is not required. |

## 3. Source of truth for naming and whys

`plugins/git-mesh/skills/handbook/sections/creating-a-mesh.md` is authoritative.
All training data, validator rules, and prompts in this project must cite it.
The training rules in §6 of this document are a *reflection* of that file. If the
two ever diverge, the handbook wins and §6 is updated.

---

## 4. The CLI contract

```bash
git mesh-recommend \
  <anchor>... \
  [--highlight "<phrase>"]... \
  [--sibling-meshes <N>] \
  [--git-context <N>]
```

- `<anchor>` is `path#Lstart-Lend` or `path` (whole-file). 2 ≤ N anchors.
- `--highlight` repeats. Each value is a short phrase (commit subject, wiki link
  text, ADR title). Defaults: none.
- `--sibling-meshes` defaults to 16. Picks the closest neighbors by prefix from
  the live `git mesh list` output.
- `--git-context` defaults to 8. Pulls commit subjects that touched ≥2 anchor
  paths from `git log --no-renames` (most-recent-first).

Output to stdout:

```json
{ "name": "billing/checkout-request-flow",
  "why":  "Checkout request flow that carries a charge attempt from the browser to the Stripe-backed server." }
```

Exit codes: `0` valid output, `2` model returned invalid output after one
repair retry, `3` tract load error, `4` tokenizer round-trip mismatch.

---

## 5. Wire format (the model's prompt)

Exactly three priority tiers plus a sentinel. No other fields.

```
@task git-mesh-relationship

@P1 highlight  api/charge.ts
@P1 highlight  Wire checkout to charge API

@P2 sibling  billing/refund-request-flow
@P2 sibling  billing/payments/stripe-webhook-replay

@P3 anchor.head  docs/billing/checkout.md#L88-L120
@P3 anchor.body  | The browser submits the charge attempt through the checkout form.
@P3 anchor.body  | The request body must match the server handler in [[src/server/api/charge.ts]].

@P3 anchor.head  src/server/api/charge.ts#L30-L76
@P3 anchor.body  | export async function handleCharge(req: ChargeRequest) {
@P3 anchor.body  |   const result = await stripe.charges.create(...)
@P3 anchor.body  | }

@out
```

Target after `@out`:

```
name: billing/checkout-request-flow
why:  Checkout request flow that carries a charge attempt from the browser to the Stripe-backed server.
```

Rules:

- Body lines start with `| ` so embedded `name:` / `@out` cannot escape (prompt-injection fence).
- Order *within* each tier: P1 by insertion, P2 by string sort, P3 anchors shuffled.
- Truncation order when over `max_source_length`: drop P3 body lines first
  (round-robin across anchors, longest-anchor first), then P2, then P1.
  `anchor.head` lines are never dropped.

---

## 6. Naming and why rules (mirrors the handbook)

### Name

- Slug: `^[a-z0-9][a-z0-9-]*(/[a-z0-9][a-z0-9-]*)*$`.
- Names the *relationship*, not either side.
- Hierarchical paths preferred once a category gets crowded:
  `billing/payments/checkout-request-flow`, `adr/0017/uuidv4-lex-order`.
- Forbidden segments: `misc`, `temp`, `stuff`, `things`, `john-work`, `frontend`,
  `backend`, `impl`, `deps`.
- Forbidden suffixes on the leaf: `-deps`, `-impl`, `-file`.
- `-link` / `-doc` allowed on prose↔prose meshes (e.g. `threat-model-controls-link`).
- No file extensions. No bare file/symbol names.
- No two adjacent equal segments.

### Why

- Exactly one sentence (single terminal `.`/`!`/`?`).
- Names the subsystem/flow/contract/rule/concern; defines what it does.
- ≤ 200 characters.
- Forbidden phrases: `don't`, `do not`, `must`, `should`, `remember to`,
  `make sure`, `review when`, `owner:`, `owned by`, `codeowners`.
- Coupling-shape detector (must not match):
  - `^X (depends on|uses|imports|calls|posts to|reads from|writes to) Y\.?$`
  - `^X (posts|sends|emits) the (shape|payload|body|format) (that|which) Y (parses|reads|consumes)\.?$`

---

## 7. Repository layout

Create exactly:

```
model-training/
  pyproject.toml
  README.md
  src/mesh_finetune/
    __init__.py
    serialize.py            # tier-prefixed wire format + deterministic truncation
    validate.py             # rules from §6, returns structured report
    mine.py                 # extracts training rows from git mesh log
    dataset.py              # JSONL loader; shuffle anchor order; tokenize
    train.py                # full fine-tune of t5-efficient-tiny
    evaluate.py             # greedy decode + all eval metrics
    export.py               # optimum-cli wrapper + onnxslim + tract preflight
    smoke_onnx.py           # ONNX Runtime debug check (not the runtime)
    tract_preflight.py      # imports tract via FFI? No — invokes the Rust binary
  prompts/
    label.md
    critique.md
    variants.md
    repair.md
  schemas/
    example.schema.json
  sample_data/
    train.jsonl
    valid.jsonl
    test.jsonl
  artifacts/
    .gitkeep

git-mesh-recommend/
  Cargo.toml
  src/
    main.rs                 # CLI; assembles the wire format from the live repo
    model.rs                # tract graph load + greedy decode loop
    tokenizer.rs            # tokenizers crate wrapper + round-trip self-test
    serialize.rs            # tier-prefixed wire format (mirrors Python)
    validate.rs             # §6 rules in Rust; runs on model output
```

The Rust binary is `git-mesh-recommend`, installed under `git-mesh`'s `bin/` so
`git mesh recommend` dispatches to it.

---

## 8. Versions to pin

In `pyproject.toml`:

```
python                  = ">=3.11,<3.13"
torch                   = "==2.5.1"
transformers            = "==4.46.0"
optimum[exporters]      = "==1.23.3"
onnx                    = "==1.17.0"
onnxslim                = "==0.1.34"          # Optimum will use this transitively
sentencepiece           = "==0.2.0"
datasets                = "==3.1.0"
pydantic                = "==2.9.2"
```

In `Cargo.toml`:

```
tract-onnx   = "0.21"          # whichever the `cargo search tract-onnx` shows current at start of work; pin in Cargo.lock
tokenizers   = "0.20"
serde        = { version = "1", features = ["derive"] }
serde_json   = "1"
clap         = { version = "4", features = ["derive"] }
anyhow       = "1"
```

If a newer `tract-onnx` exists at start of work, run §9 step 1 against both
versions; pick the higher version that passes preflight.

---

## 9. Day-by-day execution plan

Eight working days. Each day's deliverable must be runnable before the next day
starts. Stop and update this doc if a day's gate fails.

**Day 1 — tract preflight on the unmodified base model.**

1. `optimum-cli export onnx --model google/t5-efficient-tiny --task text2text-generation --opset 17 --optimize O1 model-training/artifacts/onnx-pristine`.
2. Verify output is exactly `encoder_model.onnx` + `decoder_model.onnx` (no `*_with_past_*`).
3. Build a 30-line Rust crate that calls `tract_onnx::onnx().model_for_path()` on each.
4. Run on real input shapes; confirm both graphs `into_optimized()` without error.
5. **Gate:** both graphs load and run a one-step encode/decode. If not, file the
   exact failing op and stop.

**Day 2 — Rust tokenizer round-trip.**

1. Generate `tokenizer.json` in Python:
   `transformers.convert_slow_tokenizer.SpmConverter(slow).converted().save("tokenizer.json")`.
2. Encode 50 strings (ASCII, accented, CJK, code with whitespace, empty, leading
   whitespace) in Python via `AutoTokenizer.from_pretrained(...)`.
3. Encode the same strings in Rust via `tokenizers::Tokenizer::from_file`.
4. Assert id-by-id equality.
5. **Gate:** zero mismatches. If any, document the failing string class.

**Day 3 — wire format + sample data.**

1. Implement `serialize.py` and `serialize.rs`. Property-test that the Python
   and Rust serializers produce byte-identical output for 100 random packets.
2. Hand-author 12 sample examples covering the handbook surface (see §11).
3. Implement `validate.py` (rules in §6). Run on the sample data; all 12 valid.

**Day 4 — training loop on the sample data.**

1. Implement `dataset.py`, `train.py`. Anchor-order shuffling on. Negative-example
   ratio 0.1 (rows with `output.name == "__none__"`).
2. Train 3 epochs on 12 examples. Loss must decrease.
3. **Gate:** the trained model reproduces the gold output for at least 8 of the
   12 training examples under greedy decode (overfitting is fine here; we are
   gating the loop, not generalization).

**Day 5 — export + onnxslim + tract reload.**

1. Export the trained model with the same flags as Day 1.
2. **Gate:** total artifact dir ≤ 250 MB. Print `du -sh`.
3. Reload in the Day-1 Rust crate. Both graphs still load.
4. Run greedy decode end-to-end in Rust; output must match Python's greedy
   decode for at least one held-out sample.

**Day 6 — full Rust binary.**

1. Implement `git-mesh-recommend` end-to-end (assemble packet from live repo,
   serialize, encode, greedy-decode in tract, parse, validate).
2. Run on three real meshes from this repo's `git mesh list` output.
3. **Gate:** binary exits 0 and produces handbook-compliant output on at least
   2 of 3.

**Day 7 — mining + first realistic training run.**

1. Implement `mine.py`: walks `git mesh log` of this repo, emits one row per
   mesh-version, applies the poison filters (≥1 clean commit cycle; exclude
   first 24 h after creation; exclude renamed meshes).
2. Generate first-pass labels for any mined rows that lack a why with the Claude
   prompts in `prompts/label.md` (manual paste flow; no API call required).
3. Critique with `prompts/critique.md`. Repair with `prompts/repair.md`.
4. Train for real (10 epochs, batch 8, lr 3e-4).
5. Evaluate on a held-out 20%. Report all metrics from §10.

**Day 8 — eval, polish, ship.**

1. Run the full Rust binary on a fresh `git mesh` workflow end-to-end.
2. Update `tract-smoke/README.md` with measured artifact size and the metrics
   from Day 7.
3. Tag the artifact directory.

---

## 10. Evaluation metrics

`evaluate.py` runs greedy decoding and reports:

- `name_exact` — fraction of predictions whose name string-matches the gold.
- `name_prefix` — fraction whose `<category>/[<sub>/]` matches the gold prefix.
- `taxonomy_fit` — of predictions whose top segment exists in the row's `siblings`,
  the fraction reusing a full sibling prefix.
- `name_valid` — passes §6 name rules.
- `why_valid` — passes §6 why rules.
- `why_definition_rate` — fraction of whys that pass the coupling-shape detector
  *and* contain at least one noun phrase that is *not* a verbatim anchor path.
- `both_valid` — `name_valid AND why_valid`.
- `order_stable` — for each row, run twice with different anchor orders; fraction
  with identical `(name, why)`.
- `negative_recall` — on `__none__` rows, fraction predicted as `__none__`.

Acceptance bar (after day 7):

| Metric | Target |
| --- | --- |
| `name_valid` | ≥ 0.95 |
| `why_valid` | ≥ 0.95 |
| `why_definition_rate` | ≥ 0.85 |
| `order_stable` | ≥ 0.95 |
| `negative_recall` | ≥ 0.80 |
| `taxonomy_fit` | ≥ 0.70 |
| `name_exact` | informational |

---

## 11. Sample data (12 rows)

Each row covers one of the handbook's named relationship shapes. All produced by
the team on Day 3, validated by `validate.py`, and never used as eval data.

1. Browser checkout flow ↔ server charge handler — `billing/checkout-request-flow`.
2. Tier rollout doc + dashboard + nightly recompute (3 anchors) — `experiments/tier-rollout`.
3. OAuth token doc ↔ token validator — `auth/oauth/token-refresh`.
4. Rate-limit doc ↔ middleware — `platform/rate-limits`.
5. Architecture summary ↔ ADR (whole-file × whole-file) — `docs/architecture-summary-sync`.
6. Threat-model item ↔ controls doc (whole-file × whole-file) — `security/threat-model/t-07-controls-link`.
7. CLI command docs ↔ parser/handler — `cli/command-help-parity`.
8. Migration runbook ↔ migration script — `migrations/2026-q1-user-id-backfill`.
9. Notification template doc ↔ renderer — `notifications/templates-render-contract`.
10. API contract doc ↔ request/response types — `api/charge-request-contract`.
11. ADR governing a runtime invariant ↔ code that relies on it — `adr/0017/uuidv4-lex-order`.
12. Wiki article ↔ wiki article cross-reference (whole-file × whole-file) — `wiki/world-war-ii/eastern-front`.

Plus 2 negative examples (anchors that *don't* form a mesh — e.g. two unrelated
README sections that share a word) with `output.name = "__none__"` and
`output.why = "__none__"`.

---

## 12. Mining `git mesh log`

`mine.py` walks every mesh in this repo and emits one training row per mesh-version
that satisfies all of:

- The mesh has had at least one clean `git mesh commit` cycle since creation.
- The mesh-version is at least 24 hours after the mesh's first creation.
- The mesh has never been renamed (`git mesh rename` history is empty).
- Anchor content is reachable at that mesh's commit (no `[ORPHANED]` /
  `[CONFLICT]` / `[SUBMODULE]` terminal status).

For each row:

- `anchors` from the mesh-version's anchor set, content read at the mesh commit.
- `siblings` from `git mesh list` *as of that commit*, capped at 16 closest by prefix.
- `highlights` from `git log --no-renames` subjects that touched ≥ 2 anchor paths,
  capped at the most recent 8.

Re-anchor events that preserve the same why become free order/rewrite-stability
positives.

---

## 13. Frontier LLM workflow

No API calls are required. The four prompt files are paste-targets for Claude:

- `prompts/label.md` — generates `(name, why, confidence, notes)` from a packet.
- `prompts/critique.md` — `(accept, issues, repaired)`.
- `prompts/variants.md` — rewrite-stability variants (same name + why, different
  anchors/symbols).
- `prompts/repair.md` — converts validator-failing rows into corrected rows.

Each prompt file includes the §6 rules verbatim and points the LLM at the handbook.

---

## 14. Inference contract

Runtime path inside the Rust binary:

1. Parse CLI: anchors, highlights, sibling count, git-context count.
2. Resolve each anchor (read bytes, compute line range if absent).
3. Read `git mesh list` for sibling names; pick the N closest by Levenshtein on
   path components.
4. Read `git log --no-renames -- <paths>...` for co-touching commit subjects;
   take the top N most recent subjects that touched ≥ 2 anchor paths.
5. Serialize via §5.
6. Tokenize via the `tokenizers` crate.
7. Run encoder, then greedy decode, in tract.
8. Parse the decoded text into `{name, why}`.
9. Validate via §6.
10. On invalid output, retry once with a repair-style packet (prefixed `@repair`
    plus the failing rule names).
11. On still-invalid: exit 2 with a structured error to stderr.
12. On valid: print `{name, why}` JSON to stdout, exit 0.
13. Run anchor-order shuffle once and require identical `(name, why)`; otherwise
    exit 2.

---

## 15. Acceptance

The project ships when:

1. `git-mesh-recommend` runs end-to-end on this repo and produces handbook-valid
   output on ≥ 80% of meshes mined in §12 (held out from training).
2. `du -sh git-mesh-recommend/artifacts/onnx` is ≤ 250 MB.
3. The Day-2 Python↔Rust tokenizer round-trip test passes in CI.
4. The Day-3 Python↔Rust serializer property test passes in CI.
5. `evaluate.py` reports the §10 acceptance bar.
6. `tract-preflight` is part of CI and runs on every PR that touches the model
   artifact.

If any step fails, the failure (with exact op/error/op-coverage URL) is recorded
in `git-mesh-recommend/README.md` and this document is updated before further
work proceeds.

---

## 16. Sources for the pinned decisions

- `google/t5-efficient-tiny` size — Hugging Face model card,
  https://huggingface.co/google/t5-efficient-tiny
- `google/t5-efficient-mini` size — Hugging Face model card,
  https://huggingface.co/google/t5-efficient-mini (~125 MB FP32; too large at the cap)
- tract op coverage and `~85%` ONNX backend pass — sonos/tract README,
  https://github.com/sonos/tract
- Optimum CLI export tasks — Hugging Face Optimum docs,
  https://huggingface.co/docs/optimum/exporters/onnx/usage_guides/export_a_model
- onnxslim integration into Optimum (May 2025) — onnxslim repo / PyPI,
  https://pypi.org/project/onnxslim/
- Tokenizers crate + SentencePiece via `convert_slow_tokenizer` —
  https://github.com/huggingface/tokenizers and the `convert_slow_tokenizer.SpmConverter` source.
- Handbook (authoritative naming/why rules) —
  `plugins/git-mesh/skills/handbook/sections/creating-a-mesh.md` in this repo.
