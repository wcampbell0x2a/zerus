use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
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

/// Parse `name@version` lines from all manifest files, returning the union
pub fn parse_manifests(paths: &[PathBuf]) -> anyhow::Result<BTreeSet<Crate>> {
    let mut crates = BTreeSet::new();
    for path in paths {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
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
            crates.insert(Crate::new(name.to_string(), version.to_string()));
        }
    }

    Ok(crates)
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
