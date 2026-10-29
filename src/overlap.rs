use crate::{
    changeset::{self, Change, Changeset},
    workspace,
};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::{
    ffi::OsString,
    io::Read,
    path::Path,
    process::{Command, Stdio},
};

#[derive(Serialize)]
struct Region {
    start: u64,
    length: u64,
}
#[derive(Serialize)]
struct Overlap {
    path: String,
    path_hex: String,
    workspaces: [String; 2],
    classification: &'static str,
    region_overlap: Option<bool>,
    regions: Option<[Vec<Region>; 2]>,
    incomplete: Option<String>,
}
#[derive(Serialize)]
struct Report {
    schema_version: u32,
    overlaps: Vec<Overlap>,
    warnings: Vec<String>,
}
fn path_arg(hex: &str) -> Result<OsString> {
    if !hex.len().is_multiple_of(2) {
        bail!("Invalid path encoding");
    }
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
        .collect::<std::result::Result<_, _>>()?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(OsString::from_vec(bytes))
    }
    #[cfg(not(unix))]
    {
        Ok(OsString::from(String::from_utf8(bytes)?))
    }
}
fn patch(path: &Path, base: &str, file: &str) -> Result<Vec<u8>> {
    let mut child = Command::new("git")
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-C",
        ])
        .arg(path)
        .args([
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--no-renames",
            "--no-indent-heuristic",
            "--unified=0",
            base,
            "--",
        ])
        .arg(path_arg(file)?)
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("Could not run Git")?;
    let mut out = Vec::new();
    child
        .stdout
        .take()
        .context("Missing Git output")?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut out)?;
    if out.len() > 16 * 1024 * 1024 {
        let _ = child.kill();
        let _ = child.wait();
        bail!("Diff exceeds size limit");
    }
    if !child.wait()?.success() {
        bail!("Git could not inspect file regions");
    }
    Ok(out)
}
fn regions(data: &[u8]) -> Result<Vec<Region>> {
    let mut result = Vec::new();
    for line in data.split(|b| *b == b'\n') {
        if !line.starts_with(b"@@ -") {
            continue;
        }
        let end = line[4..]
            .iter()
            .position(|b| *b == b' ')
            .context("Malformed diff hunk")?
            + 4;
        let range = std::str::from_utf8(&line[4..end])?;
        let (start, length) = match range.split_once(',') {
            Some((a, b)) => (a.parse()?, b.parse()?),
            None => (range.parse()?, 1),
        };
        result.push(Region { start, length });
    }
    Ok(result)
}
fn touches(a: &Region, b: &Region) -> bool {
    let end = |r: &Region| r.start.saturating_add(r.length.saturating_sub(1));
    a.start <= end(b).saturating_add(1) && b.start <= end(a).saturating_add(1)
}
fn names_match(a: &Change, b: &Change) -> bool {
    a.path_hex == b.path_hex
        || a.previous_path_hex
            .as_ref()
            .is_some_and(|p| p == &b.path_hex || b.previous_path_hex.as_ref() == Some(p))
        || b.previous_path_hex.as_ref() == Some(&a.path_hex)
}
fn analyze(a: &Changeset, b: &Changeset, ca: &Change, cb: &Change) -> Overlap {
    let mut o = Overlap {
        path: ca.path.clone(),
        path_hex: ca.path_hex.clone(),
        workspaces: [a.name.clone(), b.name.clone()],
        classification: "file_overlap",
        region_overlap: None,
        regions: None,
        incomplete: None,
    };
    let reason = if a.base_commit != b.base_commit {
        Some("different base commits")
    } else if ca.previous_path_hex.is_some() || cb.previous_path_hex.is_some() {
        Some("rename or copy provenance is ambiguous")
    } else if ca.binary == Some(true) || cb.binary == Some(true) {
        Some("binary file")
    } else if ca.incomplete.is_some() || cb.incomplete.is_some() {
        Some("file content is incomplete")
    } else if ca.kind == "deleted" || cb.kind == "deleted" {
        o.classification = "delete_modify";
        Some("deleted file has no comparable regions")
    } else if ca.kind == "added" && cb.kind == "added" {
        o.classification = "add_add";
        Some("added files have no shared original regions")
    } else if ca.kind == "added" || cb.kind == "added" {
        Some("addition and another change have ambiguous provenance")
    } else if ca.kind != "modified" || cb.kind != "modified" {
        Some("file type or change kind is ambiguous")
    } else {
        None
    };
    if let Some(reason) = reason {
        o.incomplete = Some(reason.into());
        return o;
    }
    let get = |s: &Changeset, c: &Change| -> Result<Vec<Region>> {
        let record = workspace::get(&s.name)?;
        let p = Path::new(&record.path);
        let first = patch(p, &s.base_commit, &c.path_hex)?;
        let second = patch(p, &s.base_commit, &c.path_hex)?;
        if first != second {
            bail!("File changed during region analysis");
        }
        let r = regions(&first)?;
        if r.is_empty() {
            bail!("No comparable text hunks (possibly a mode-only change)");
        }
        Ok(r)
    };
    match (get(a, ca), get(b, cb)) {
        (Ok(x), Ok(y)) => {
            let overlap = x.iter().any(|r| y.iter().any(|s| touches(r, s)));
            o.region_overlap = Some(overlap);
            o.classification = if overlap {
                "region_overlap"
            } else {
                "separate_regions"
            };
            o.regions = Some([x, y]);
        }
        (Err(e), _) | (_, Err(e)) => o.incomplete = Some(e.to_string()),
    };
    o
}
pub(crate) fn show(requested: &[String], json: bool) -> Result<()> {
    let names = if requested.is_empty() {
        workspace::names()?
    } else {
        requested.to_vec()
    };
    let mut unique = std::collections::HashSet::new();
    for name in &names {
        if !unique.insert(name) {
            bail!("Duplicate workspace name: {name}");
        }
    }
    let sets: Vec<Changeset> = names
        .iter()
        .map(|n| changeset::collect(n))
        .collect::<Result<_>>()?;
    let mut overlaps = Vec::new();
    for i in 0..sets.len() {
        for j in i + 1..sets.len() {
            for a in &sets[i].aggregate_changes {
                for b in &sets[j].aggregate_changes {
                    if names_match(a, b) {
                        overlaps.push(analyze(&sets[i], &sets[j], a, b));
                    }
                }
            }
        }
    }
    let report = Report { schema_version: 1, overlaps, warnings: vec!["Overlap is an observation, not a confirmed Git conflict or proof of semantic compatibility. Collection is not an atomic snapshot.".into()] };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        if report.overlaps.is_empty() {
            println!("No shared changed files.");
        }
        for o in &report.overlaps {
            println!(
                "{}: {} and {} — {}{}",
                o.path.escape_default(),
                o.workspaces[0],
                o.workspaces[1],
                o.classification,
                o.incomplete
                    .as_ref()
                    .map(|s| format!(" (incomplete: {s})"))
                    .unwrap_or_default()
            );
        }
        eprintln!(
            "Overlap does not establish whether Git can merge changes or whether they are semantically compatible."
        );
    }
    Ok(())
}
