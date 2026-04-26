//! `git mesh` list / `git mesh <name>` show / `git mesh ls` — §10.4, §3.4.

use crate::cli::{LsArgs, ShowArgs, parse_range_address};
use crate::range::read_range;
use crate::types::{Range, RangeExtent};
use crate::{
    MeshCommitInfo, list_mesh_names, ls_all, ls_by_path, ls_by_path_range, mesh_commit_info,
    mesh_commit_info_at, mesh_log, read_mesh, read_mesh_at,
};
use anyhow::Result;

// ---------------------------------------------------------------------------
// Format-string types
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FormatToken {
    Literal(String),
    Newline,
    Commit(CommitField),
    Range(RangeField),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CommitField {
    /// `%H` — full mesh commit SHA
    CommitHash,
    /// `%h` — 7-char abbreviated mesh commit SHA
    CommitHashShort,
    /// `%an` — author name
    AuthorName,
    /// `%ae` — author email
    AuthorEmail,
    /// `%ad` — author date (RFC 2822)
    AuthorDate,
    /// `%ar` — author date, relative
    AuthorDateRelative,
    /// `%s` — subject (first line of message)
    Subject,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RangeField {
    /// `%p` — range path
    Path,
    /// `%r` — range extent specifier (`#L<s>-L<e>` or empty for whole-file)
    RangeSpec,
    /// `%P` — path + range spec
    PathWithSpec,
    /// `%a` — anchor SHA (full)
    AnchorFull,
}

const SUPPORTED: &str = "%H, %h, %an, %ae, %ad, %ar, %s, %p, %r, %P, %a";

/// Parse a format string into a vector of tokens, returning an error for
/// any unknown placeholder.
pub(crate) fn parse_format(fmt: &str) -> anyhow::Result<Vec<FormatToken>> {
    let mut tokens: Vec<FormatToken> = Vec::new();
    let mut literal = String::new();
    let mut chars = fmt.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '%' {
            literal.push(c);
            continue;
        }
        let Some(&nc) = chars.peek() else {
            // Trailing lone `%` — treat as literal.
            literal.push('%');
            break;
        };
        match nc {
            '%' => {
                chars.next();
                literal.push('%');
            }
            'n' => {
                chars.next();
                if !literal.is_empty() {
                    tokens.push(FormatToken::Literal(std::mem::take(&mut literal)));
                }
                tokens.push(FormatToken::Newline);
            }
            'H' => {
                chars.next();
                if !literal.is_empty() {
                    tokens.push(FormatToken::Literal(std::mem::take(&mut literal)));
                }
                tokens.push(FormatToken::Commit(CommitField::CommitHash));
            }
            'h' => {
                chars.next();
                if !literal.is_empty() {
                    tokens.push(FormatToken::Literal(std::mem::take(&mut literal)));
                }
                tokens.push(FormatToken::Commit(CommitField::CommitHashShort));
            }
            's' => {
                chars.next();
                if !literal.is_empty() {
                    tokens.push(FormatToken::Literal(std::mem::take(&mut literal)));
                }
                tokens.push(FormatToken::Commit(CommitField::Subject));
            }
            'p' => {
                chars.next();
                if !literal.is_empty() {
                    tokens.push(FormatToken::Literal(std::mem::take(&mut literal)));
                }
                tokens.push(FormatToken::Range(RangeField::Path));
            }
            'r' => {
                chars.next();
                if !literal.is_empty() {
                    tokens.push(FormatToken::Literal(std::mem::take(&mut literal)));
                }
                tokens.push(FormatToken::Range(RangeField::RangeSpec));
            }
            'P' => {
                chars.next();
                if !literal.is_empty() {
                    tokens.push(FormatToken::Literal(std::mem::take(&mut literal)));
                }
                tokens.push(FormatToken::Range(RangeField::PathWithSpec));
            }
            'a' => {
                // Could be `%an`, `%ae`, `%ad`, `%ar`, or `%a` (anchor full).
                chars.next(); // consume 'a'
                let sub = chars.peek().copied();
                match sub {
                    Some('n') => {
                        chars.next();
                        if !literal.is_empty() {
                            tokens.push(FormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(FormatToken::Commit(CommitField::AuthorName));
                    }
                    Some('e') => {
                        chars.next();
                        if !literal.is_empty() {
                            tokens.push(FormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(FormatToken::Commit(CommitField::AuthorEmail));
                    }
                    Some('d') => {
                        chars.next();
                        if !literal.is_empty() {
                            tokens.push(FormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(FormatToken::Commit(CommitField::AuthorDate));
                    }
                    Some('r') => {
                        chars.next();
                        if !literal.is_empty() {
                            tokens.push(FormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(FormatToken::Commit(CommitField::AuthorDateRelative));
                    }
                    // `%a` alone (no recognized sub-char) → anchor SHA full
                    None | Some(_) => {
                        // peek was already consumed for 'a'; we need to check if
                        // the next char could form a known two-char token.
                        // Only emit Range::AnchorFull if next char is NOT 'n','e','d','r'
                        // (those were handled above). Since we've already peeked and those
                        // cases didn't match, this is `%a` with something unrecognized after it
                        // OR end of string. Treat standalone `%a` as anchor full.
                        if !literal.is_empty() {
                            tokens.push(FormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        // But we must check if the next char makes an unknown 2-char seq.
                        // At this point `sub` = chars.peek() — if it's a letter, that's an unknown.
                        if let Some(s) = sub
                            && s.is_ascii_alphabetic()
                        {
                            // Unknown `%a<X>` sequence.
                            chars.next(); // consume the unknown sub-char
                            let tok = format!("a{s}");
                            return Err(anyhow::anyhow!(
                                "unknown format placeholder \"%{tok}\"; supported: {SUPPORTED}"
                            ));
                        }
                        tokens.push(FormatToken::Range(RangeField::AnchorFull));
                    }
                }
            }
            other => {
                chars.next();
                // Check if this is a multi-char unknown like `%xx` — we already consumed
                // the first char after `%`, so emit error for this single-char unknown.
                // But we should also accumulate subsequent chars for better error messages.
                // For now: report the single unknown char.
                return Err(anyhow::anyhow!(
                    "unknown format placeholder \"%{other}\"; supported: {SUPPORTED}"
                ));
            }
        }
    }

    if !literal.is_empty() {
        tokens.push(FormatToken::Literal(literal));
    }

    Ok(tokens)
}

fn has_range_token(tokens: &[FormatToken]) -> bool {
    tokens
        .iter()
        .any(|t| matches!(t, FormatToken::Range(_)))
}

/// Render a single line from the token vector against the mesh commit info and
/// an optional range context. Range tokens require `range` to be `Some`.
pub(crate) fn render_tokens(
    tokens: &[FormatToken],
    info: &MeshCommitInfo,
    meta: &crate::git::CommitMeta,
    range: Option<&Range>,
) -> String {
    let mut out = String::new();
    for tok in tokens {
        match tok {
            FormatToken::Literal(s) => out.push_str(s),
            FormatToken::Newline => out.push('\n'),
            FormatToken::Commit(f) => match f {
                CommitField::CommitHash => out.push_str(&info.commit_oid),
                CommitField::CommitHashShort => {
                    out.push_str(&info.commit_oid[..7.min(info.commit_oid.len())]);
                }
                CommitField::AuthorName => out.push_str(&meta.author_name),
                CommitField::AuthorEmail => out.push_str(&meta.author_email),
                CommitField::AuthorDate => out.push_str(&meta.author_date_rfc2822),
                CommitField::AuthorDateRelative => out.push_str(
                    &crate::cli::stale_output::format_relative(meta.committer_time),
                ),
                CommitField::Subject => out.push_str(&meta.summary),
            },
            FormatToken::Range(f) => {
                let r = range.expect("range token present but no range context — invariant violated");
                match f {
                    RangeField::Path => out.push_str(&r.path),
                    RangeField::RangeSpec => {
                        if let RangeExtent::Lines { start, end } = r.extent {
                            out.push_str(&format!("#L{start}-L{end}"));
                        }
                        // Whole-file → empty string (no push)
                    }
                    RangeField::PathWithSpec => {
                        out.push_str(&r.path);
                        if let RangeExtent::Lines { start, end } = r.extent {
                            out.push_str(&format!("#L{start}-L{end}"));
                        }
                    }
                    RangeField::AnchorFull => out.push_str(&r.anchor_sha),
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Public run functions
// ---------------------------------------------------------------------------

pub fn run_list(repo: &gix::Repository) -> Result<i32> {
    let names = list_mesh_names(repo)?;
    if names.is_empty() {
        println!("no meshes");
        return Ok(0);
    }
    for name in names {
        let m = read_mesh(repo, &name)?;
        let summary = m.message.lines().next().unwrap_or_default();
        println!("{name}\t{} ranges\t{summary}", m.ranges.len());
    }
    Ok(0)
}

pub fn run_show(repo: &gix::Repository, args: ShowArgs) -> Result<i32> {
    if args.log {
        let entries = mesh_log(repo, &args.name, args.limit)?;
        for info in entries {
            if args.oneline {
                println!("{} {}", short(&info.commit_oid), info.summary);
            } else {
                println!("commit {}", info.commit_oid);
                println!("Author: {} <{}>", info.author_name, info.author_email);
                println!("Date:   {}", info.author_date);
                println!();
                for line in info.message.trim_end_matches('\n').lines() {
                    println!("    {line}");
                }
                println!();
            }
        }
        return Ok(0);
    }

    let mesh = read_mesh_at(repo, &args.name, args.at.as_deref())?;
    let info = mesh_commit_info_at(repo, &args.name, args.at.as_deref())?;

    // --format=<FMT> short-circuits the default rendering (§10.4).
    if let Some(fmt) = &args.format {
        let tokens = match parse_format(fmt) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("git-mesh: {e}");
                return Ok(2);
            }
        };

        let meta = crate::git::commit_meta(repo, &info.commit_oid)
            .map_err(|e| anyhow::anyhow!("commit meta: {e}"))?;

        if has_range_token(&tokens) {
            for id in &mesh.ranges {
                let r = read_range(repo, id)?;
                let line = render_tokens(&tokens, &info, &meta, Some(&r));
                println!("{line}");
            }
        } else {
            let line = render_tokens(&tokens, &info, &meta, None);
            println!("{line}");
        }
        return Ok(0);
    }

    if args.oneline {
        for id in &mesh.ranges {
            let r = read_range(repo, id)?;
            println!("{}", render_range_address(&r.path, r.extent));
        }
        return Ok(0);
    }

    println!("mesh {}", mesh.name);
    println!("commit {}", info.commit_oid);
    println!("Author: {} <{}>", info.author_name, info.author_email);
    println!("Date:   {}", info.author_date);
    println!();
    for line in mesh.message.trim_end_matches('\n').lines() {
        println!("    {line}");
    }
    println!();
    println!("Ranges ({}):", mesh.ranges.len());
    for id in &mesh.ranges {
        let r = read_range(repo, id)?;
        println!("    {}", render_range_address(&r.path, r.extent));
    }

    // Consume unused field warning via bind.
    let _ = mesh_commit_info(repo, &args.name);
    Ok(0)
}

pub fn run_ls(repo: &gix::Repository, args: LsArgs) -> Result<i32> {
    let entries = match args.target {
        None => ls_all(repo)?,
        Some(t) => {
            if t.contains("#L") {
                let (path, s, e) = parse_range_address(&t)?;
                ls_by_path_range(repo, &path, s, e)?
            } else {
                ls_by_path(repo, &t)?
            }
        }
    };
    for e in entries {
        println!("{}\t{}\t{}-{}", e.path, e.mesh_name, e.start, e.end);
    }
    Ok(0)
}

fn render_range_address(path: &str, extent: RangeExtent) -> String {
    match extent {
        RangeExtent::Lines { start, end } => format!("{path}#L{start}-L{end}"),
        RangeExtent::Whole => format!("{path}  (whole)"),
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::CommitMeta;
    use crate::types::RangeExtent;

    fn fake_info() -> MeshCommitInfo {
        MeshCommitInfo {
            commit_oid: "abcdef1234567890abcdef1234567890abcdef12".to_string(),
            author_name: "Alice Author".to_string(),
            author_email: "alice@example.com".to_string(),
            author_date: "Mon, 01 Jan 2024 00:00:00 +0000".to_string(),
            summary: "the subject line".to_string(),
            message: "the subject line\n\nbody".to_string(),
        }
    }

    fn fake_meta() -> CommitMeta {
        CommitMeta {
            author_name: "Alice Author".to_string(),
            author_email: "alice@example.com".to_string(),
            author_date_rfc2822: "Mon, 01 Jan 2024 00:00:00 +0000".to_string(),
            committer_time: 1704067200,
            summary: "the subject line".to_string(),
            message: "the subject line\n\nbody".to_string(),
        }
    }

    fn fake_range_lines() -> Range {
        Range {
            anchor_sha: "deadbeef1234567890abcdef1234567890abcdef".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            path: "src/foo.rs".to_string(),
            extent: RangeExtent::Lines { start: 10, end: 20 },
            blob: "bloboid1".to_string(),
        }
    }

    fn fake_range_whole() -> Range {
        Range {
            anchor_sha: "cafebabe1234567890abcdef1234567890abcdef".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            path: "docs/guide.md".to_string(),
            extent: RangeExtent::Whole,
            blob: "bloboid2".to_string(),
        }
    }

    #[test]
    fn commit_placeholder_big_h() {
        let tokens = parse_format("%H").unwrap();
        let out = render_tokens(&tokens, &fake_info(), &fake_meta(), None);
        assert_eq!(out, "abcdef1234567890abcdef1234567890abcdef12");
    }

    #[test]
    fn commit_placeholder_h_abbrev() {
        let tokens = parse_format("%h").unwrap();
        let out = render_tokens(&tokens, &fake_info(), &fake_meta(), None);
        assert_eq!(out, "abcdef1");
    }

    #[test]
    fn commit_placeholder_s() {
        let tokens = parse_format("%s").unwrap();
        let out = render_tokens(&tokens, &fake_info(), &fake_meta(), None);
        assert_eq!(out, "the subject line");
    }

    #[test]
    fn commit_placeholder_an() {
        let tokens = parse_format("%an").unwrap();
        let out = render_tokens(&tokens, &fake_info(), &fake_meta(), None);
        assert_eq!(out, "Alice Author");
    }

    #[test]
    fn commit_placeholder_ae() {
        let tokens = parse_format("%ae").unwrap();
        let out = render_tokens(&tokens, &fake_info(), &fake_meta(), None);
        assert_eq!(out, "alice@example.com");
    }

    #[test]
    fn commit_placeholder_ad() {
        let tokens = parse_format("%ad").unwrap();
        let out = render_tokens(&tokens, &fake_info(), &fake_meta(), None);
        assert_eq!(out, "Mon, 01 Jan 2024 00:00:00 +0000");
    }

    #[test]
    fn commit_placeholder_ar_produces_relative_time() {
        let tokens = parse_format("%ar").unwrap();
        let out = render_tokens(&tokens, &fake_info(), &fake_meta(), None);
        // We just check it's non-empty; the exact relative string depends on wall time.
        assert!(!out.is_empty());
    }

    #[test]
    fn range_placeholder_p_lines() {
        let tokens = parse_format("%p").unwrap();
        let r = fake_range_lines();
        let out = render_tokens(&tokens, &fake_info(), &fake_meta(), Some(&r));
        assert_eq!(out, "src/foo.rs");
    }

    #[test]
    fn range_placeholder_r_lines() {
        let tokens = parse_format("%r").unwrap();
        let r = fake_range_lines();
        let out = render_tokens(&tokens, &fake_info(), &fake_meta(), Some(&r));
        assert_eq!(out, "#L10-L20");
    }

    #[test]
    fn range_placeholder_r_whole_is_empty() {
        let tokens = parse_format("%r").unwrap();
        let r = fake_range_whole();
        let out = render_tokens(&tokens, &fake_info(), &fake_meta(), Some(&r));
        assert_eq!(out, "");
    }

    #[test]
    fn range_placeholder_big_p_lines() {
        let tokens = parse_format("%P").unwrap();
        let r = fake_range_lines();
        let out = render_tokens(&tokens, &fake_info(), &fake_meta(), Some(&r));
        assert_eq!(out, "src/foo.rs#L10-L20");
    }

    #[test]
    fn range_placeholder_big_p_whole_is_just_path() {
        let tokens = parse_format("%P").unwrap();
        let r = fake_range_whole();
        let out = render_tokens(&tokens, &fake_info(), &fake_meta(), Some(&r));
        assert_eq!(out, "docs/guide.md");
    }

    #[test]
    fn range_placeholder_a_full() {
        let tokens = parse_format("%a").unwrap();
        let r = fake_range_lines();
        let out = render_tokens(&tokens, &fake_info(), &fake_meta(), Some(&r));
        assert_eq!(out, "deadbeef1234567890abcdef1234567890abcdef");
    }

    #[test]
    fn percent_percent_escapes_literal() {
        let tokens = parse_format("100%%").unwrap();
        let out = render_tokens(&tokens, &fake_info(), &fake_meta(), None);
        assert_eq!(out, "100%");
    }

    #[test]
    fn percent_n_is_newline() {
        let tokens = parse_format("a%nb").unwrap();
        let out = render_tokens(&tokens, &fake_info(), &fake_meta(), None);
        assert_eq!(out, "a\nb");
    }

    #[test]
    fn has_range_token_true_for_range_placeholders() {
        assert!(has_range_token(&parse_format("%p").unwrap()));
        assert!(has_range_token(&parse_format("%r").unwrap()));
        assert!(has_range_token(&parse_format("%P").unwrap()));
        assert!(has_range_token(&parse_format("%a").unwrap()));
    }

    #[test]
    fn has_range_token_false_for_commit_only() {
        assert!(!has_range_token(&parse_format("%H %s %an").unwrap()));
    }

    #[test]
    fn unknown_placeholder_big_s_rejected() {
        let err = parse_format("%S").unwrap_err();
        assert!(err.to_string().contains("%S"), "{err}");
        assert!(err.to_string().contains("supported:"), "{err}");
    }

    #[test]
    fn unknown_placeholder_xx_rejected() {
        // %x → unknown single char
        let err = parse_format("%x").unwrap_err();
        assert!(err.to_string().contains("supported:"), "{err}");
    }

    #[test]
    fn unknown_placeholder_az_rejected() {
        let err = parse_format("%aZ").unwrap_err();
        assert!(err.to_string().contains("supported:"), "{err}");
    }
}
