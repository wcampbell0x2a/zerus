use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use rayon::prelude::*;
use tracing::{debug, info};

use crate::index::find_crate_files;
use crate::{get_crate_path, Crate};

/// Collect all crates in the mirror, derived from the `crates/{prefix}/{name}/{version}/` layout
pub fn generate(mirror_path: &Path) -> anyhow::Result<Vec<Crate>> {
    let crates_path = mirror_path.join("crates");
    if !crates_path.is_dir() {
        bail!("no crates directory found at {}", crates_path.display());
    }

    let mut crates = BTreeSet::new();
    for path in find_crate_files(&crates_path) {
        let Some(version) = path.parent().and_then(|p| p.file_name()) else {
            bail!("unexpected crate file path: {}", path.display());
        };
        let Some(name) = path
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.file_name())
        else {
            bail!("unexpected crate file path: {}", path.display());
        };
        crates.insert(Crate::new(
            name.to_string_lossy().into_owned(),
            version.to_string_lossy().into_owned(),
        ));
    }

    Ok(crates.into_iter().collect())
}

/// Write `name@version` lines to `output`, or stdout if not given
pub fn write_manifest(crates: &[Crate], output: Option<&Path>) -> anyhow::Result<()> {
    let mut buf = String::new();
    for c in crates {
        buf.push_str(&c.name);
        buf.push('@');
        buf.push_str(&c.version);
        buf.push('\n');
    }

    match output {
        Some(path) => {
            fs::write(path, buf).with_context(|| format!("failed to write {}", path.display()))?;
            info!("wrote {} crate(s) to {}", crates.len(), path.display());
        }
        None => {
            std::io::stdout().write_all(buf.as_bytes())?;
        }
    }

    Ok(())
}

/// Parse `name@version` lines from a single manifest file, in file order
fn parse_one(path: &Path) -> anyhow::Result<Vec<Crate>> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;

    let mut crates = Vec::new();
    for (n, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, version)) = line.split_once('@') else {
            bail!(
                "{}:{}: expected name@version, found {line:?}",
                path.display(),
                n + 1
            );
        };
        if name.is_empty() || version.is_empty() {
            bail!(
                "{}:{}: expected name@version, found {line:?}",
                path.display(),
                n + 1
            );
        }
        crates.push(Crate::new(name.to_string(), version.to_string()));
    }

    Ok(crates)
}

/// Parse `name@version` lines from all manifest files, returning the union
pub fn parse_manifests(paths: &[PathBuf]) -> anyhow::Result<BTreeSet<Crate>> {
    let mut crates = BTreeSet::new();
    for path in paths {
        crates.extend(parse_one(path)?);
    }

    Ok(crates)
}

/// A single manifest file and the result of parsing it. A file that fails to parse keeps its
/// error, so it does not hide the files that parse.
#[derive(Debug)]
pub struct ParsedFile {
    /// File name as-is, e.g. `2026-08-13.txt`. Treated as an opaque label
    pub name: String,
    pub crates: anyhow::Result<Vec<Crate>>,
}

/// Crates in `mirror` that no manifest records, i.e. crates that never made a trip
pub fn unmanifested<'m, 'r>(
    mirror: impl IntoIterator<Item = &'m Crate>,
    recorded: impl IntoIterator<Item = &'r Crate>,
) -> Vec<&'m Crate> {
    let recorded: BTreeSet<&Crate> = recorded.into_iter().collect();
    mirror
        .into_iter()
        .filter(|c| !recorded.contains(*c))
        .collect()
}

/// Manifest files in `dir` as `(file name, path)` pairs, in no set order.
/// Dotfiles and subdirectories are not manifests, so they are skipped.
fn list_dir(dir: &Path) -> anyhow::Result<Vec<(String, PathBuf)>> {
    let entries = fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))?;

    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("failed to read {}", dir.display()))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }

        files.push((name, path));
    }

    Ok(files)
}

/// Parse each manifest file in `dir` on its own, sorted by file name.
/// Fails only when `dir` itself cannot be read.
pub fn parse_each(dir: &Path) -> anyhow::Result<Vec<ParsedFile>> {
    // Each read is a round trip on a network mount, so do them in parallel
    let mut files: Vec<ParsedFile> = list_dir(dir)?
        .into_par_iter()
        .map(|(name, path)| ParsedFile {
            crates: parse_one(&path),
            name,
        })
        .collect();

    files.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(files)
}

/// Remove crates listed in the manifests from the mirror so they aren't transferred again
pub fn cull(mirror_path: &Path, manifests: &[PathBuf], dry_run: bool) -> anyhow::Result<()> {
    let crates = parse_manifests(manifests)?;
    let crates_path = mirror_path.join("crates");

    let mut removed = 0;
    let mut missing = 0;
    for c in &crates {
        let Some(dir) = get_crate_path(mirror_path, &c.name, &c.version) else {
            bail!("invalid crate name: {}", c.name);
        };
        let crate_path = dir.join(format!("{}-{}.crate", c.name, c.version));
        if !crate_path.is_file() {
            missing += 1;
            continue;
        }

        if dry_run {
            info!("would remove {}", crate_path.display());
        } else {
            debug!("removing {}", crate_path.display());
            fs::remove_file(&crate_path)
                .with_context(|| format!("failed to remove {}", crate_path.display()))?;
            remove_empty_dirs(&dir, &crates_path);
        }
        removed += 1;
    }

    let action = if dry_run { "would remove" } else { "removed" };
    info!(
        "{action} {removed} crate(s) ({} listed, {missing} not present)",
        crates.len()
    );

    Ok(())
}

/// Remove `dir` and its parents while empty, stopping at `stop` (exclusive)
fn remove_empty_dirs(dir: &Path, stop: &Path) {
    let mut dir = dir;
    while dir != stop && fs::remove_dir(dir).is_ok() {
        let Some(parent) = dir.parent() else {
            return;
        };
        dir = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names_of<'a>(crates: impl IntoIterator<Item = &'a Crate>) -> Vec<String> {
        crates
            .into_iter()
            .map(|c| format!("{}@{}", c.name, c.version))
            .collect()
    }

    /// The crates of a file that must parse
    fn parsed(file: &ParsedFile) -> Vec<String> {
        names_of(file.crates.as_ref().unwrap())
    }

    #[test]
    fn parse_each_attributes_crates_to_each_file() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("2026-08-13.txt"),
            "serde@1.0.210\naxum@0.8.1\n",
        )
        .unwrap();
        fs::write(tmp.path().join("2026-06-01.txt"), "serde@1.0.204\n").unwrap();

        let files = parse_each(tmp.path()).unwrap();

        // sorted by file name, and each file keeps its own crates in file order
        assert_eq!(
            files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            ["2026-06-01.txt", "2026-08-13.txt"]
        );
        assert_eq!(parsed(&files[0]), ["serde@1.0.204"]);
        assert_eq!(parsed(&files[1]), ["serde@1.0.210", "axum@0.8.1"]);
    }

    #[test]
    fn parse_each_skips_comments_blanks_dotfiles_and_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("transfer.txt"),
            "# generated by something\n\nserde@1.0.210\n\n   \n",
        )
        .unwrap();
        fs::write(tmp.path().join(".hidden.txt"), "tokio@1.40.0\n").unwrap();
        fs::create_dir(tmp.path().join("subdir")).unwrap();

        let files = parse_each(tmp.path()).unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "transfer.txt");
        assert_eq!(parsed(&files[0]), ["serde@1.0.210"]);
    }

    #[test]
    fn parse_each_keeps_a_bad_file_apart_from_the_good_ones() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("bad.txt"),
            "serde@1.0.210\nnot-a-crate-line\n",
        )
        .unwrap();
        fs::write(tmp.path().join("good.txt"), "serde@1.0.210\n").unwrap();

        let files = parse_each(tmp.path()).unwrap();

        let err = files[0].crates.as_ref().unwrap_err().to_string();
        assert!(err.contains("bad.txt:2"), "unexpected error: {err}");
        assert_eq!(parsed(&files[1]), ["serde@1.0.210"]);
    }

    #[test]
    fn parse_each_rejects_an_empty_name_or_version() {
        let tmp = tempfile::tempdir().unwrap();
        for (file, line) in [("a.txt", "@1.0.0"), ("b.txt", "serde@")] {
            fs::write(tmp.path().join(file), format!("{line}\n")).unwrap();
        }

        let files = parse_each(tmp.path()).unwrap();

        assert!(files.iter().all(|f| f.crates.is_err()));
    }

    #[test]
    fn parse_each_on_empty_dir_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(parse_each(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn parse_each_on_a_missing_dir_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(parse_each(&tmp.path().join("nope")).is_err());
    }

    /// Put a `.crate` file in the mirror at the layout `generate` expects
    fn add_crate(mirror: &Path, name: &str, version: &str) {
        let dir = get_crate_path(mirror, name, version).unwrap();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{name}-{version}.crate")), b"x").unwrap();
    }

    fn crates(list: &[(&str, &str)]) -> Vec<Crate> {
        list.iter()
            .map(|(n, v)| Crate::new(n.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn generate_lists_the_crate_files_in_name_order() {
        let tmp = tempfile::tempdir().unwrap();
        add_crate(tmp.path(), "tokio", "1.40.0");
        add_crate(tmp.path(), "serde", "1.0.210");

        let mirror = generate(tmp.path()).unwrap();

        assert_eq!(names_of(&mirror), ["serde@1.0.210", "tokio@1.40.0"]);
    }

    #[test]
    fn generate_on_a_mirror_with_no_crates_dir_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(generate(tmp.path()).is_err());
    }

    #[test]
    fn unmanifested_finds_mirror_crates_that_no_manifest_records() {
        let mirror = crates(&[
            ("axum", "0.8.1"),
            ("serde", "1.0.210"),
            ("tokio", "1.40.0"),
        ]);
        let recorded = crates(&[("serde", "1.0.210"), ("axum", "0.8.1")]);

        assert_eq!(
            names_of(unmanifested(&mirror, &recorded)),
            ["tokio@1.40.0"]
        );
    }

    #[test]
    fn unmanifested_matches_on_version_not_name_alone() {
        let mirror = crates(&[("serde", "1.0.204"), ("serde", "1.0.210")]);
        // A manifest recording one version leaves the other un-manifested.
        let recorded = crates(&[("serde", "1.0.204")]);

        assert_eq!(
            names_of(unmanifested(&mirror, &recorded)),
            ["serde@1.0.210"]
        );
    }

    #[test]
    fn unmanifested_with_no_manifests_is_the_whole_mirror() {
        let mirror = crates(&[("serde", "1.0.210"), ("tokio", "1.40.0")]);

        assert_eq!(
            names_of(unmanifested(&mirror, &crates(&[]))),
            ["serde@1.0.210", "tokio@1.40.0"]
        );
    }

    #[test]
    fn unmanifested_ignores_manifest_entries_absent_from_the_mirror() {
        let mirror = crates(&[("serde", "1.0.210")]);
        // `axum` was culled, so it is in a manifest but not in the mirror.
        let recorded = crates(&[("axum", "0.8.1")]);

        assert_eq!(
            names_of(unmanifested(&mirror, &recorded)),
            ["serde@1.0.210"]
        );
    }

    #[test]
    fn parse_manifests_still_unions_and_dedupes() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.txt");
        let b = tmp.path().join("b.txt");
        fs::write(&a, "serde@1.0.210\ntokio@1.40.0\n").unwrap();
        fs::write(&b, "serde@1.0.210\naxum@0.8.1\n").unwrap();

        let crates = parse_manifests(&[a, b]).unwrap();

        assert_eq!(
            crates
                .iter()
                .map(|c| format!("{}@{}", c.name, c.version))
                .collect::<Vec<_>>(),
            ["axum@0.8.1", "serde@1.0.210", "tokio@1.40.0"]
        );
    }
}
