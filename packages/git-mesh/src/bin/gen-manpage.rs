//! Generator for `git-mesh.1`.
//!
//! Writes a roff manpage to the path given as `argv[1]`, defaulting to
//! `$CARGO_MANIFEST_DIR/man/git-mesh.1` when no argument is supplied.
//!
//! Run via `cargo run --bin gen-manpage -- [<output-path>]`.

use std::io::Write as _;
use std::path::PathBuf;

use clap::CommandFactory as _;
use git_mesh::cli::Cli;

fn main() -> anyhow::Result<()> {
    let out_path: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
                .unwrap_or_else(|_| ".".to_string());
            PathBuf::from(manifest_dir).join("man").join("git-mesh.1")
        });

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let cmd = Cli::command();
    let man = clap_mangen::Man::new(cmd);

    let mut buf: Vec<u8> = Vec::new();

    // Render standard sections from clap_mangen.
    man.render_title(&mut buf)?;
    man.render_name_section(&mut buf)?;
    man.render_synopsis_section(&mut buf)?;

    // Hand-authored DESCRIPTION — replaces the one-line clap `about` string and
    // adds the full mesh contract. The `after_help` block (EXTRA section) is
    // intentionally omitted here; it belongs in `--help` output, not the manpage.
    write!(buf, "{}", DESCRIPTION_SECTION)?;

    man.render_options_section(&mut buf)?;
    man.render_subcommands_section(&mut buf)?;
    man.render_version_section(&mut buf)?;

    // Hand-authored EXAMPLES section.
    write!(buf, "{}", EXAMPLES_SECTION)?;

    // SEE ALSO section.
    write!(buf, "{}", SEE_ALSO_SECTION)?;

    std::fs::write(&out_path, &buf)?;
    Ok(())
}

/// Hand-authored DESCRIPTION section.
///
/// Defines the mesh contract — what a mesh is, the why's role, and re-anchoring
/// on drift.
const DESCRIPTION_SECTION: &str = r#".SH DESCRIPTION
.B git mesh
tracks implicit semantic dependencies in a Git repository: couplings between
line ranges (or whole files), in code or prose, that no schema, type, or test
enforces.
Each
.B mesh
anchors the participating ranges and carries a durable
.I why
\[em] one prose sentence that names the relationship the anchored ranges hold
and survives a rewrite of either side.
.PP
The standing question at commit time: did this change create or rely on a
coupling that is not visible from the lines themselves?
.PP
.B Writing the why.
Name the relationship the ranges hold in one prose sentence, written so it
survives a rewrite of either side.
Describe the relationship in role\-words (\(lqthe doc,\(rq \(lqthe parser,\(rq
\(lqthe runbook,\(rq \(lqthe migration\(rq) rather than repeating filenames.
For asymmetric relationships, name which side is normative:
\(lqthe doc is the source of truth when they disagree.\(rq
Do not restate the mesh name, embed incidental implementation details, or
bundle ownership and review triggers in the why.
.PP
.B Re\-anchoring on drift.
When
.B git mesh stale
reports drift, review the change at each anchor.
If the relationship still holds, re-anchor with
.B git mesh add
and commit.
If the relationship has changed, update the why with
.B git mesh why
before re-anchoring.
.PP
Meshes are stored as Git refs under
.IR refs/meshes/v1/<name> ,
so they travel with
.B git fetch
and
.B git push
like branches and tags.
.B git mesh fetch
and
.B git mesh push
keep remote mirrors in sync.
.PP
Bare invocations:
.RS 4
.TP
.B git mesh
List every mesh in the repository.
.TP
.BI git\ mesh\  <name>
Show one mesh (ranges, why, config).
.RE
"#;

/// Hand-authored EXAMPLES section.
const EXAMPLES_SECTION: &str = r#".SH EXAMPLES
Anchor a new mesh alongside a code change:
.PP
.RS 4
.nf
git mesh add billing/charge-request-contract \e
    docs/api/charge.md#L40-L88 api/charge.ts#L30-L76
git mesh why billing/charge-request-contract \e
    -m "The doc states the request body shape the parser honors; \e
the doc is the source of truth when they disagree."
git commit -m "Wire checkout to charge API"
# post-commit hook runs: git mesh commit
.fi
.RE
.PP
Document an existing relationship anchored at HEAD:
.PP
.RS 4
.nf
git mesh add auth/token-contract --at HEAD \e
    packages/auth/token.ts#L88-L104 \e
    packages/auth/crypto.ts#L12-L40
git mesh why auth/token-contract \e
    -m "Token verification depends on signature verification."
git mesh commit auth/token-contract
.fi
.RE
.PP
Check for drift and inspect a mesh:
.PP
.RS 4
.nf
git mesh stale
git mesh billing/charge-request-contract
git mesh billing/charge-request-contract --log
.fi
.RE
"#;

/// SEE ALSO section.
const SEE_ALSO_SECTION: &str = r#".SH SEE ALSO
.BR git (1),
.BR gitcli (7)
"#;
