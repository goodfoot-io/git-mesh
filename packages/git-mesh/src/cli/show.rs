//! `git mesh` list / `git mesh <name>` show / `git mesh list` — §10.4, §3.4.

use crate::cli::{ListArgs, ShowArgs, parse_range_address};
use crate::staging::{list_staged_mesh_names, read_staging};
use crate::types::{Anchor, AnchorExtent};
use crate::validation::validate_mesh_name_shape;
use crate::{MeshCommitInfo, mesh_commit_info_at, mesh_log, read_mesh_at};
use anyhow::Result;
use regex::RegexBuilder;
use std::collections::HashSet;
use std::io::{self, BufRead};

// ---------------------------------------------------------------------------
// Format-string types
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FormatToken {
    Literal(String),
    Newline,
    Commit(CommitField),
    Anchor(AnchorField),
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
pub(crate) enum AnchorField {
    /// `%p` — anchor path
    Path,
    /// `%r` — anchor extent specifier (`#L<s>-L<e>` or empty for whole-file)
    RangeSpec,
    /// `%P` — path + anchor spec
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
                tokens.push(FormatToken::Anchor(AnchorField::Path));
            }
            'r' => {
                chars.next();
                if !literal.is_empty() {
                    tokens.push(FormatToken::Literal(std::mem::take(&mut literal)));
                }
                tokens.push(FormatToken::Anchor(AnchorField::RangeSpec));
            }
            'P' => {
                chars.next();
                if !literal.is_empty() {
                    tokens.push(FormatToken::Literal(std::mem::take(&mut literal)));
                }
                tokens.push(FormatToken::Anchor(AnchorField::PathWithSpec));
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
                        // Only emit Anchor::AnchorFull if next char is NOT 'n','e','d','r'
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
                        tokens.push(FormatToken::Anchor(AnchorField::AnchorFull));
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
    tokens.iter().any(|t| matches!(t, FormatToken::Anchor(_)))
}

/// Render a single line from the token vector against the mesh commit info and
/// an optional anchor context. Anchor tokens require `anchor` to be `Some`.
pub(crate) fn render_tokens(
    tokens: &[FormatToken],
    info: &MeshCommitInfo,
    meta: &crate::git::CommitMeta,
    anchor: Option<&Anchor>,
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
            FormatToken::Anchor(f) => {
                let r = anchor
                    .expect("anchor token present but no anchor context — invariant violated");
                match f {
                    AnchorField::Path => out.push_str(&r.path),
                    AnchorField::RangeSpec => {
                        if let AnchorExtent::LineRange { start, end } = r.extent {
                            out.push_str(&format!("#L{start}-L{end}"));
                        }
                        // Whole-file → empty string (no push)
                    }
                    AnchorField::PathWithSpec => {
                        out.push_str(&r.path);
                        if let AnchorExtent::LineRange { start, end } = r.extent {
                            out.push_str(&format!("#L{start}-L{end}"));
                        }
                    }
                    AnchorField::AnchorFull => out.push_str(&r.anchor_sha),
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Listing pipeline types and helpers
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
enum MeshState {
    Committed,
    Staged,
    Pending,
}

#[derive(Clone)]
struct AnchorEntry {
    path: String,
    extent: AnchorExtent,
}

#[derive(Clone)]
struct MeshListing {
    name: String,
    why: String,
    anchors: Vec<AnchorEntry>,
    state: MeshState,
}

fn collect_listings(repo: &gix::Repository) -> Result<Vec<MeshListing>> {
    collect_listings_with_options(repo, true, true)
}

fn collect_listings_with_options(
    repo: &gix::Repository,
    include_why: bool,
    include_state: bool,
) -> Result<Vec<MeshListing>> {
    let committed_refs = {
        let _perf = crate::perf::span("list.list-committed-meshes");
        crate::mesh::read::list_mesh_refs(repo)?
    };
    let committed_names: Vec<&str> = committed_refs
        .iter()
        .map(|(name, _oid)| name.as_str())
        .collect();
    let staged_names = {
        let _perf = crate::perf::span("list.list-staged-meshes");
        list_staged_mesh_names(repo)?
    };

    let staged_name_set: HashSet<&str> = staged_names.iter().map(String::as_str).collect();
    let committed_name_set: HashSet<&str> = committed_names.iter().copied().collect();
    let mut listings: Vec<MeshListing> =
        Vec::with_capacity(committed_refs.len() + staged_names.len());
    // Collect committed meshes.
    {
        let _perf = crate::perf::span("list.read-committed-meshes");
        for (name, commit_oid) in &committed_refs {
            let (message, anchors_v2) = if include_why {
                let mesh = crate::mesh::read::read_mesh_listing_at(repo, commit_oid)?;
                (mesh.message, mesh.anchors_v2)
            } else {
                (
                    String::new(),
                    crate::mesh::read::read_anchors_v2_blob(repo, commit_oid).unwrap_or_default(),
                )
            };
            let mut anchors = Vec::new();
            for (_id, r) in anchors_v2 {
                anchors.push(AnchorEntry {
                    path: r.path,
                    extent: r.extent,
                });
            }
            // Determine if this committed mesh also has staged ops.
            let state = if include_state && staged_name_set.contains(name.as_str()) {
                let staging = read_staging(repo, name)?;
                if staging.adds.is_empty()
                    && staging.removes.is_empty()
                    && staging.configs.is_empty()
                    && staging.why.is_none()
                {
                    MeshState::Committed
                } else {
                    MeshState::Staged
                }
            } else {
                MeshState::Committed
            };
            let why = message.trim_end_matches('\n').to_string();
            listings.push(MeshListing {
                name: name.clone(),
                why,
                anchors,
                state,
            });
        }
    }

    // Collect staging-only (pending) meshes.
    {
        let _perf = crate::perf::span("list.read-pending-meshes");
        for name in &staged_names {
            if committed_name_set.contains(name.as_str()) {
                continue; // already handled above
            }
            let staging = read_staging(repo, name)?;
            let why = staging.why.unwrap_or_default();
            let anchors = staging
                .adds
                .into_iter()
                .map(|a| AnchorEntry {
                    path: a.path,
                    extent: a.extent,
                })
                .collect();
            listings.push(MeshListing {
                name: name.clone(),
                why,
                anchors,
                state: MeshState::Pending,
            });
        }
    }

    Ok(listings)
}

fn collect_listings_for_names(
    repo: &gix::Repository,
    names: &[String],
    include_why: bool,
    include_state: bool,
) -> Result<Vec<MeshListing>> {
    let name_set: HashSet<&str> = names.iter().map(String::as_str).collect();

    let committed_refs = crate::mesh::read::list_mesh_refs(repo)?;
    let staged_names = list_staged_mesh_names(repo)?;

    let staged_name_set: HashSet<&str> = staged_names.iter().map(String::as_str).collect();
    let committed_name_set: HashSet<&str> = committed_refs.iter().map(|(n, _)| n.as_str()).collect();

    let mut listings = Vec::with_capacity(names.len());

    // Committed meshes matching the name set.
    for (name, commit_oid) in &committed_refs {
        if !name_set.contains(name.as_str()) {
            continue;
        }
        let (message, anchors_v2) = if include_why {
            let mesh = crate::mesh::read::read_mesh_listing_at(repo, commit_oid)?;
            (mesh.message, mesh.anchors_v2)
        } else {
            (
                String::new(),
                crate::mesh::read::read_anchors_v2_blob(repo, commit_oid).unwrap_or_default(),
            )
        };
        let mut anchors = Vec::new();
        for (_id, r) in anchors_v2 {
            anchors.push(AnchorEntry {
                path: r.path,
                extent: r.extent,
            });
        }
        // Determine if this committed mesh also has staged ops.
        let state = if include_state && staged_name_set.contains(name.as_str()) {
            let staging = read_staging(repo, name)?;
            if staging.adds.is_empty()
                && staging.removes.is_empty()
                && staging.configs.is_empty()
                && staging.why.is_none()
            {
                MeshState::Committed
            } else {
                MeshState::Staged
            }
        } else {
            MeshState::Committed
        };
        let why = message.trim_end_matches('\n').to_string();
        listings.push(MeshListing {
            name: name.clone(),
            why,
            anchors,
            state,
        });
    }

    // Pending meshes matching the name set.
    for name in &staged_names {
        if !name_set.contains(name.as_str()) {
            continue;
        }
        if committed_name_set.contains(name.as_str()) {
            continue; // already handled as committed+staged
        }
        let staging = read_staging(repo, name)?;
        let why = staging.why.unwrap_or_default();
        let anchors = staging
            .adds
            .into_iter()
            .map(|a| AnchorEntry {
                path: a.path,
                extent: a.extent,
            })
            .collect();
        listings.push(MeshListing {
            name: name.clone(),
            why,
            anchors,
            state: MeshState::Pending,
        });
    }

    Ok(listings)
}

fn collect_filtered_porcelain_listings_with_staging(
    repo: &gix::Repository,
    target: &str,
    staged_listings: Option<&[MeshListing]>,
) -> Result<Vec<MeshListing>> {
    let (path, range) = if target.contains("#L") {
        let (path, start, end) = parse_range_address(target)?;
        (path, Some((start, end)))
    } else {
        (target.to_string(), None)
    };

    let committed_names = {
        let _perf = crate::perf::span("list.path-index-lookup");
        crate::mesh::path_index::matching_mesh_names(repo, &path, range)?
    };
    let mut listings = Vec::with_capacity(committed_names.len());
    {
        let _perf = crate::perf::span("list.path-index-candidate-expansion");
        for name in committed_names {
            let commit_oid = match crate::mesh::read::resolve_mesh_revision(repo, &name, None) {
                Ok(commit_oid) => commit_oid,
                Err(crate::Error::MeshNotFound(_)) => continue,
                Err(err) => return Err(err.into()),
            };
            let anchors = crate::mesh::read::read_anchors_v2_blob(repo, &commit_oid)
                .unwrap_or_default()
                .into_iter()
                .map(|(_id, anchor)| AnchorEntry {
                    path: anchor.path,
                    extent: anchor.extent,
                })
                .collect();
            listings.push(MeshListing {
                name,
                why: String::new(),
                anchors,
                state: MeshState::Committed,
            });
        }
    }

    {
        let _perf = crate::perf::span("list.path-index-pending-meshes");
        let staged;
        let staged_listings = match staged_listings {
            Some(staged_listings) => staged_listings,
            None => {
                staged = collect_staged_porcelain_listings(repo)?;
                &staged
            }
        };
        for listing in staged_listings {
            if listing
                .anchors
                .iter()
                .any(|anchor| anchor_matches(anchor, &path, range))
            {
                listings.push(listing.clone());
            }
        }
    }

    Ok(listings)
}

fn collect_staged_porcelain_listings(repo: &gix::Repository) -> Result<Vec<MeshListing>> {
    let staged_names = list_staged_mesh_names(repo)?;
    let mut listings = Vec::with_capacity(staged_names.len());
    for name in staged_names {
        let staging = read_staging(repo, &name)?;
        let anchors: Vec<AnchorEntry> = staging
            .adds
            .into_iter()
            .map(|add| AnchorEntry {
                path: add.path,
                extent: add.extent,
            })
            .collect();
        listings.push(MeshListing {
            name,
            why: String::new(),
            anchors,
            state: MeshState::Pending,
        });
    }
    Ok(listings)
}

fn anchor_matches(anchor: &AnchorEntry, path: &str, range: Option<(u32, u32)>) -> bool {
    if anchor.path != path {
        return false;
    }
    match (anchor.extent, range) {
        (_, None) => true,
        (AnchorExtent::WholeFile, Some(_)) => true,
        (AnchorExtent::LineRange { start, end }, Some((query_start, query_end))) => {
            start <= query_end && end >= query_start
        }
    }
}

fn apply_search(listings: &mut Vec<MeshListing>, re: &regex::Regex) {
    listings.retain(|l| {
        if re.is_match(&l.name) {
            return true;
        }
        for line in l.why.lines() {
            if re.is_match(line) {
                return true;
            }
        }
        for a in &l.anchors {
            if re.is_match(&a.path) {
                return true;
            }
            let addr = render_range_address(&a.path, a.extent);
            if re.is_match(&addr) {
                return true;
            }
        }
        false
    });
}

fn render_blocks(page: &[MeshListing]) {
    let total = page.len();
    for (i, listing) in page.iter().enumerate() {
        let marker = match listing.state {
            MeshState::Committed => String::new(),
            MeshState::Staged => " (staged)".to_string(),
            MeshState::Pending => " (pending)".to_string(),
        };
        println!("{}{}:", listing.name, marker);
        for a in &listing.anchors {
            let addr = match a.extent {
                AnchorExtent::LineRange { start, end } => {
                    format!("{}#L{start}-L{end}", a.path)
                }
                AnchorExtent::WholeFile => a.path.clone(),
            };
            println!("- {addr}");
        }
        let trimmed_why = listing.why.trim();
        if !trimmed_why.is_empty() {
            println!();
            println!("{trimmed_why}");
        }
        if i + 1 < total {
            println!();
            println!("---");
            println!();
        }
    }
}

fn render_porcelain(page: &[MeshListing]) {
    for listing in page {
        for a in &listing.anchors {
            let extent_str = match a.extent {
                AnchorExtent::LineRange { start, end } => format!("{start}-{end}"),
                AnchorExtent::WholeFile => "0-0".to_string(),
            };
            println!("{}\t{}\t{}", listing.name, a.path, extent_str);
        }
    }
}

fn run_list_batch_porcelain(repo: &gix::Repository) -> Result<i32> {
    let staged_listings = {
        let _perf = crate::perf::span("list.batch-read-staged-meshes");
        collect_staged_porcelain_listings(repo)?
    };
    let stdin = io::stdin();
    for target in stdin.lock().lines() {
        let target = target?;
        let mut listings = collect_filtered_porcelain_listings_with_staging(
            repo,
            &target,
            Some(&staged_listings),
        )?;
        listings.sort_by(|a, b| a.name.cmp(&b.name));

        if listings.is_empty() {
            println!("no meshes");
        } else {
            render_porcelain(&listings);
        }
    }

    Ok(0)
}

// ---------------------------------------------------------------------------
// Public run functions
// ---------------------------------------------------------------------------

pub fn run_show(repo: &gix::Repository, args: ShowArgs) -> Result<i32> {
    if args.log {
        let entries = {
            let _perf = crate::perf::span("show.read-log");
            mesh_log(repo, &args.name, args.limit)?
        };
        let _perf = crate::perf::span("show.render-log");
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

    let mesh = {
        let _perf = crate::perf::span("show.read-mesh");
        read_mesh_at(repo, &args.name, args.at.as_deref())?
    };
    let info = {
        let _perf = crate::perf::span("show.read-commit-info");
        mesh_commit_info_at(repo, &args.name, args.at.as_deref())?
    };

    // --format=<FMT> short-circuits the default rendering (§10.4).
    if let Some(fmt) = &args.format {
        let tokens = {
            let _perf = crate::perf::span("show.parse-format");
            match parse_format(fmt) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("git-mesh: {e}");
                    return Ok(2);
                }
            }
        };

        let meta = {
            let _perf = crate::perf::span("show.read-commit-meta");
            crate::git::commit_meta(repo, &info.commit_oid)
                .map_err(|e| anyhow::anyhow!("commit meta: {e}"))?
        };

        let _perf = crate::perf::span("show.render-format");
        if has_range_token(&tokens) {
            for (_id, r) in &mesh.anchors_v2 {
                let line = render_tokens(&tokens, &info, &meta, Some(r));
                println!("{line}");
            }
        } else {
            let line = render_tokens(&tokens, &info, &meta, None);
            println!("{line}");
        }
        return Ok(0);
    }

    if args.oneline {
        let _perf = crate::perf::span("show.render-oneline");
        for (_id, r) in &mesh.anchors_v2 {
            println!("{}", render_range_address(&r.path, r.extent));
        }
        return Ok(0);
    }

    let _perf = crate::perf::span("show.render-default");
    println!("mesh {}", mesh.name);
    println!("commit {}", info.commit_oid);
    println!("Author: {} <{}>", info.author_name, info.author_email);
    println!("Date:   {}", info.author_date);
    println!();
    for line in mesh.message.trim_end_matches('\n').lines() {
        println!("    {line}");
    }
    println!();
    println!("Anchors ({}):", mesh.anchors_v2.len());
    for (_id, r) in &mesh.anchors_v2 {
        println!("    {}", render_range_address(&r.path, r.extent));
    }
    Ok(0)
}

pub fn run_list(repo: &gix::Repository, args: ListArgs) -> Result<i32> {
    if args.batch {
        let _perf = crate::perf::span("list.batch-porcelain");
        return run_list_batch_porcelain(repo);
    }

    // Resolve targets to mesh names (or list all if no args).
    let resolved_names: Option<Vec<String>> = if args.targets.is_empty() {
        None
    } else {
        match resolve_targets(repo, &args.targets) {
            Ok(names) => Some(names),
            Err(_e) => {
                // stderr diagnostics already printed by resolve_targets
                return Ok(1);
            }
        }
    };

    let include_why = !args.porcelain || args.search.is_some();
    let include_state = !args.porcelain;
    let mut listings = {
        let _perf = crate::perf::span("list.collect");
        if let Some(ref names) = resolved_names {
            collect_listings_for_names(repo, names, include_why, include_state)?
        } else if include_why && include_state {
            collect_listings(repo)?
        } else {
            collect_listings_with_options(repo, include_why, include_state)?
        }
    };

    if let Some(ref pat) = args.search {
        let _perf = crate::perf::span("list.filter-search");
        match RegexBuilder::new(pat).case_insensitive(true).build() {
            Ok(re) => apply_search(&mut listings, &re),
            Err(err) => {
                eprintln!("git-mesh: invalid --search pattern `{pat}`: {err}");
                return Ok(2);
            }
        }
    }

    let _perf = crate::perf::span("list.sort-page-render");
    listings.sort_by(|a, b| a.name.cmp(&b.name));

    let page: Vec<_> = listings
        .into_iter()
        .skip(args.offset)
        .take(args.limit.unwrap_or(usize::MAX))
        .collect();

    if page.is_empty() {
        println!("no meshes");
        return Ok(0);
    }

    if args.porcelain {
        render_porcelain(&page);
    } else {
        render_blocks(&page);
    }

    Ok(0)
}

fn render_range_address(path: &str, extent: AnchorExtent) -> String {
    match extent {
        AnchorExtent::LineRange { start, end } => format!("{path}#L{start}-L{end}"),
        AnchorExtent::WholeFile => format!("{path}  (whole)"),
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

// ---------------------------------------------------------------------------
// Multi-target resolver
// ---------------------------------------------------------------------------

/// Resolve positional args to a deduplicated set of mesh names.
///
/// Two-step dispatch per arg:
///   - `#L` range → `parse_range_address` + `matching_mesh_names` with range
///   - `/` present → `matching_mesh_names` without range (skip mesh-name check)
///   - bare arg → check `refs/meshes/v1/<arg>`; if exists, use as mesh name,
///     else fall through to `matching_mesh_names` without range
///
/// Zero-match args produce stderr diagnostics and an error.  Empty args return
/// an empty vector immediately.
pub(crate) fn resolve_targets(
    repo: &gix::Repository,
    args: &[String],
) -> Result<Vec<String>> {
    if args.is_empty() {
        return Ok(Vec::new());
    }

    let mut result: HashSet<String> = HashSet::new();
    let mut missing_args: Vec<&str> = Vec::new();

    // Hoist the staging-name set so we can check for staging-only meshes in
    // the bare-arg dispatch without re-reading per arg.
    let staged_names: HashSet<String> = list_staged_mesh_names(repo)?
        .into_iter()
        .collect();

    // Resolve each arg. Rule: a zero-match against the mesh set is fine on
    // its own — exit 0 silently. We only error when the arg names something
    // that doesn't exist: a missing file, a missing mesh name, or a glob the
    // shell left literal because it matched nothing.
    for arg in args {
        if arg.contains("#L") {
            // Range address: parse path and range, look up via path index.
            let (path, start, end) = parse_range_address(arg)?;
            let names = crate::mesh::path_index::matching_mesh_names(repo, &path, Some((start, end)))?;
            if !names.is_empty() {
                result.extend(names);
            } else if !file_exists_in_workdir(repo, std::path::Path::new(&path)) {
                missing_args.push(arg);
            }
        } else if validate_mesh_name_shape(arg).is_ok() {
            // Mesh-name shape (bare slug or hierarchical): try mesh name
            // first (committed → staging), fall back to path index, then
            // fall back to a worktree existence check.
            let ref_name = format!("refs/meshes/v1/{arg}");
            if crate::git::resolve_ref_oid_optional_repo(repo, &ref_name)?.is_some() {
                result.insert(arg.clone());
            } else if staged_names.contains(arg.as_str()) {
                // Staging-only mesh (has staged ops but no committed ref yet).
                result.insert(arg.clone());
            } else {
                let names =
                    crate::mesh::path_index::matching_mesh_names(repo, arg, None)?;
                if !names.is_empty() {
                    result.extend(names);
                } else if !file_exists_in_workdir(repo, std::path::Path::new(arg)) {
                    missing_args.push(arg);
                }
            }
        } else {
            // Not mesh-name shape (path with extension, glob): go straight
            // to path index.
            let names = crate::mesh::path_index::matching_mesh_names(repo, arg, None)?;
            if !names.is_empty() {
                result.extend(names);
            } else if !file_exists_in_workdir(repo, std::path::Path::new(arg)) {
                missing_args.push(arg);
            }
        }
    }

    if !missing_args.is_empty() {
        for arg in &missing_args {
            eprintln!("git mesh list: no such file or mesh: '{arg}'");
        }
        anyhow::bail!("one or more targets named no existing file or mesh");
    }

    Ok(result.into_iter().collect())
}

fn file_exists_in_workdir(repo: &gix::Repository, rel: &std::path::Path) -> bool {
    repo.workdir()
        .map(|w| w.join(rel).exists())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::CommitMeta;
    use crate::types::AnchorExtent;

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

    fn fake_range_lines() -> Anchor {
        Anchor {
            anchor_sha: "deadbeef1234567890abcdef1234567890abcdef".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            path: "src/foo.rs".to_string(),
            extent: AnchorExtent::LineRange { start: 10, end: 20 },
            blob: "bloboid1".to_string(),
        }
    }

    fn fake_range_whole() -> Anchor {
        Anchor {
            anchor_sha: "cafebabe1234567890abcdef1234567890abcdef".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            path: "docs/guide.md".to_string(),
            extent: AnchorExtent::WholeFile,
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
    fn anchor_placeholder_p_lines() {
        let tokens = parse_format("%p").unwrap();
        let r = fake_range_lines();
        let out = render_tokens(&tokens, &fake_info(), &fake_meta(), Some(&r));
        assert_eq!(out, "src/foo.rs");
    }

    #[test]
    fn anchor_placeholder_r_lines() {
        let tokens = parse_format("%r").unwrap();
        let r = fake_range_lines();
        let out = render_tokens(&tokens, &fake_info(), &fake_meta(), Some(&r));
        assert_eq!(out, "#L10-L20");
    }

    #[test]
    fn anchor_placeholder_r_whole_is_empty() {
        let tokens = parse_format("%r").unwrap();
        let r = fake_range_whole();
        let out = render_tokens(&tokens, &fake_info(), &fake_meta(), Some(&r));
        assert_eq!(out, "");
    }

    #[test]
    fn anchor_placeholder_big_p_lines() {
        let tokens = parse_format("%P").unwrap();
        let r = fake_range_lines();
        let out = render_tokens(&tokens, &fake_info(), &fake_meta(), Some(&r));
        assert_eq!(out, "src/foo.rs#L10-L20");
    }

    #[test]
    fn anchor_placeholder_big_p_whole_is_just_path() {
        let tokens = parse_format("%P").unwrap();
        let r = fake_range_whole();
        let out = render_tokens(&tokens, &fake_info(), &fake_meta(), Some(&r));
        assert_eq!(out, "docs/guide.md");
    }

    #[test]
    fn anchor_placeholder_a_full() {
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
    fn has_anchor_token_true_for_anchor_placeholders() {
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

    // -----------------------------------------------------------------------
    // resolve_targets helpers and tests
    // -----------------------------------------------------------------------

    use crate::git::{apply_ref_transaction_repo, RefUpdate};
    use crate::mesh::path_index::ref_updates_for_mesh;
    use crate::types::Anchor;
    use std::path::Path;
    use std::process::Command;

    fn seed_repo() -> (tempfile::TempDir, gix::Repository) {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path();
        run_git(dir, &["init", "--initial-branch=main"]);
        run_git(dir, &["config", "user.email", "t@t"]);
        run_git(dir, &["config", "user.name", "t"]);
        run_git(dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("a.txt"), "alpha\n").unwrap();
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-m", "init"]);
        let repo = gix::open(dir).unwrap();
        (td, repo)
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn anchor(path: &str, start: u32, end: u32) -> Anchor {
        Anchor {
            anchor_sha: "0000000000000000000000000000000000000000".to_string(),
            created_at: "2025-01-01T00:00:00Z".to_string(),
            path: path.to_string(),
            extent: AnchorExtent::LineRange { start, end },
            blob: "0000000000000000000000000000000000000000".to_string(),
        }
    }

    fn create_mesh_ref(repo: &gix::Repository, name: &str) {
        let head_oid = repo
            .head_id()
            .unwrap()
            .detach()
            .to_string();
        let updates = vec![RefUpdate::Create {
            name: format!("refs/meshes/v1/{name}"),
            new_oid: head_oid,
        }];
        apply_ref_transaction_repo(repo, &updates).unwrap();
    }

    fn create_path_index_entry(
        repo: &gix::Repository,
        mesh_name: &str,
        path: &str,
        start: u32,
        end: u32,
    ) {
        let anchors = vec![("a1".to_string(), anchor(path, start, end))];
        let updates = ref_updates_for_mesh(repo, mesh_name, &[], &anchors).unwrap();
        apply_ref_transaction_repo(repo, &updates).unwrap();
    }

    #[test]
    fn resolve_targets_finds_mesh_by_name() {
        let (_td, repo) = seed_repo();
        create_mesh_ref(&repo, "my-mesh");
        let result = resolve_targets(&repo, &["my-mesh".to_string()]).unwrap();
        assert_eq!(result, vec!["my-mesh"]);
    }

    #[test]
    fn resolve_targets_path_index_lookup() {
        let (_td, repo) = seed_repo();
        create_path_index_entry(&repo, "mesh-a", "a.txt", 1, 5);
        let result = resolve_targets(&repo, &["a.txt".to_string()]).unwrap();
        assert_eq!(result, vec!["mesh-a"]);
    }

    #[test]
    fn resolve_targets_hash_l_range() {
        let (_td, repo) = seed_repo();
        create_path_index_entry(&repo, "mesh-a", "a.txt", 1, 10);
        let result =
            resolve_targets(&repo, &["a.txt#L1-L5".to_string()]).unwrap();
        assert_eq!(result, vec!["mesh-a"]);
    }

    #[test]
    fn resolve_targets_non_mesh_name_shape_goes_to_path_index() {
        // src/lib.rs contains `.rs` which fails mesh-name shape validation,
        // so it must go straight to path index even though it contains `/`.
        let (_td, repo) = seed_repo();
        create_path_index_entry(&repo, "mesh-from-path", "src/lib.rs", 1, 10);
        let result =
            resolve_targets(&repo, &["src/lib.rs".to_string()]).unwrap();
        assert_eq!(result, vec!["mesh-from-path"]);
    }

    #[test]
    fn resolve_targets_hierarchical_name_falls_through_when_no_mesh() {
        let (_td, repo) = seed_repo();
        // "category/slug" matches mesh-name shape but no mesh ref or
        // staging entry exists. Must fall through to path index.
        create_path_index_entry(&repo, "some-mesh", "category/slug", 1, 5);
        let result =
            resolve_targets(&repo, &["category/slug".to_string()]).unwrap();
        assert_eq!(result, vec!["some-mesh"]);
    }

    #[test]
    fn resolve_targets_deduplicates_across_args() {
        let (_td, repo) = seed_repo();
        create_mesh_ref(&repo, "mesh-a");
        create_path_index_entry(&repo, "mesh-a", "a.txt", 1, 5);
        let result = resolve_targets(
            &repo,
            &["mesh-a".to_string(), "a.txt".to_string()],
        )
        .unwrap();
        assert_eq!(result, vec!["mesh-a"]);
    }

    #[test]
    fn resolve_targets_zero_match_errors() {
        let (_td, repo) = seed_repo();
        let err = resolve_targets(&repo, &["nonexistent".to_string()])
            .unwrap_err();
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn resolve_targets_empty_args_returns_empty_vec() {
        let (_td, repo) = seed_repo();
        let result: Vec<String> = resolve_targets(&repo, &[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn resolve_targets_mixed_mesh_names_and_paths() {
        let (_td, repo) = seed_repo();
        create_mesh_ref(&repo, "mesh-a");
        create_path_index_entry(&repo, "mesh-b", "a.txt", 1, 5);
        let result = resolve_targets(
            &repo,
            &["mesh-a".to_string(), "a.txt".to_string()],
        )
        .unwrap();
        let mut sorted = result.clone();
        sorted.sort();
        assert_eq!(sorted, vec!["mesh-a", "mesh-b"]);
    }

    #[test]
    fn resolve_targets_staging_only_mesh() {
        let (_td, repo) = seed_repo();
        // Create a staging-only mesh (no committed ref) by writing an ops file
        // into .git/mesh/staging/ — same layout write_staging produces.
        let staging_dir = repo.git_dir().join("mesh").join("staging");
        std::fs::create_dir_all(&staging_dir).unwrap();
        std::fs::write(staging_dir.join("pending-mesh"), "add a.txt#L1-L5\n").unwrap();
        let result = resolve_targets(&repo, &["pending-mesh".to_string()]).unwrap();
        assert_eq!(result, vec!["pending-mesh"]);
    }

    #[test]
    fn resolve_targets_committed_wins_over_staging() {
        // When a mesh has both a committed ref and staging entries, the
        // committed ref takes priority (same name, no duplication).
        let (_td, repo) = seed_repo();
        create_mesh_ref(&repo, "dual-mesh");
        let staging_dir = repo.git_dir().join("mesh").join("staging");
        std::fs::create_dir_all(&staging_dir).unwrap();
        std::fs::write(staging_dir.join("dual-mesh"), "add a.txt#L1-L5\n").unwrap();
        let result = resolve_targets(&repo, &["dual-mesh".to_string()]).unwrap();
        assert_eq!(result, vec!["dual-mesh"]);
    }

    // -----------------------------------------------------------------------
    // run_list integration tests
    // -----------------------------------------------------------------------

    #[test]
    fn run_list_no_args_returns_ok() {
        let (_td, repo) = seed_repo();
        let args = ListArgs {
            targets: vec![],
            porcelain: false,
            batch: false,
            search: None,
            offset: 0,
            limit: None,
        };
        let exit_code = run_list(&repo, args).unwrap();
        assert_eq!(exit_code, 0);
    }

    #[test]
    fn run_list_mesh_name_arg_returns_ok() {
        let (_td, repo) = seed_repo();
        create_mesh_ref(&repo, "my-mesh");
        let args = ListArgs {
            targets: vec!["my-mesh".to_string()],
            porcelain: false,
            batch: false,
            search: None,
            offset: 0,
            limit: None,
        };
        let exit_code = run_list(&repo, args).unwrap();
        assert_eq!(exit_code, 0);
    }

    #[test]
    fn run_list_zero_match_returns_exit_1() {
        let (_td, repo) = seed_repo();
        let args = ListArgs {
            targets: vec!["nonexistent".to_string()],
            porcelain: false,
            batch: false,
            search: None,
            offset: 0,
            limit: None,
        };
        let exit_code = run_list(&repo, args).unwrap();
        assert_eq!(exit_code, 1);
    }

    #[test]
    fn resolve_targets_works_after_rename() {
        // Simulate the failing integration test: commit a mesh, rename it,
        // then look up via path index.
        let (_td, repo) = seed_repo();

        // Commit mesh "alpha" with file1.txt#L1-L5.
        create_mesh_ref(&repo, "alpha");
        create_path_index_entry(&repo, "alpha", "a.txt", 1, 5);

        // Rename "alpha" → "renamed" via direct ref + path-index ops.
        let old_ref = "refs/meshes/v1/alpha";
        let new_ref = "refs/meshes/v1/renamed";
        let head_oid = repo.head_id().unwrap().detach().to_string();
        let updates = vec![
            RefUpdate::Create {
                name: new_ref.to_string(),
                new_oid: head_oid.clone(),
            },
            RefUpdate::Delete {
                name: old_ref.to_string(),
                expected_old_oid: head_oid,
            },
        ];
        apply_ref_transaction_repo(&repo, &updates).unwrap();

        // Update path index from "alpha" to "renamed" via ref_updates_for_rename,
        // which atomically removes old entries and adds new ones.
        let anchors = vec![("a1".to_string(), anchor("a.txt", 1, 5))];
        let pi_updates =
            crate::mesh::path_index::ref_updates_for_rename(&repo, "alpha", "renamed", &anchors)
                .unwrap();
        apply_ref_transaction_repo(&repo, &pi_updates).unwrap();

        // Now resolve_targets should find only "renamed" via the path index.
        let result = resolve_targets(&repo, &["a.txt#L3-L4".to_string()]).unwrap();
        assert_eq!(result, vec!["renamed"]);
    }

    #[test]
    fn run_list_multiple_mesh_names() {
        let (_td, repo) = seed_repo();
        create_mesh_ref(&repo, "mesh-a");
        create_mesh_ref(&repo, "mesh-b");
        let args = ListArgs {
            targets: vec!["mesh-a".to_string(), "mesh-b".to_string()],
            porcelain: false,
            batch: false,
            search: None,
            offset: 0,
            limit: None,
        };
        let exit_code = run_list(&repo, args).unwrap();
        assert_eq!(exit_code, 0);
    }

    // --- hierarchical mesh name reproduction tests ---

    #[test]
    fn resolve_targets_hierarchical_name_resolves_to_mesh() {
        let (_td, repo) = seed_repo();
        create_mesh_ref(&repo, "billing/payments/checkout");
        let result =
            resolve_targets(&repo, &["billing/payments/checkout".to_string()]).unwrap();
        assert_eq!(result, vec!["billing/payments/checkout"]);
    }

    #[test]
    fn resolve_targets_hierarchical_name_staging_only() {
        let (_td, repo) = seed_repo();
        let staging_dir = repo.git_dir().join("mesh").join("staging");
        std::fs::create_dir_all(&staging_dir).unwrap();
        std::fs::write(
            staging_dir.join("billing%2Fpayments%2Fcheckout"),
            "add a.txt#L1-L5\n",
        )
        .unwrap();
        let result =
            resolve_targets(&repo, &["billing/payments/checkout".to_string()]).unwrap();
        assert_eq!(result, vec!["billing/payments/checkout"]);
    }
}
