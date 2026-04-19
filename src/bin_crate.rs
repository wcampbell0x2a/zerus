use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context};
use reqwest::blocking::Client;
use serde::Deserialize;
use tempfile::TempDir;

use crate::get_index_prefix;

pub fn parse_spec(spec: &str) -> (&str, Option<&str>) {
    match spec.split_once('@') {
        Some((name, version)) => (name, Some(version)),
        None => (spec, None),
    }
}

pub fn resolve_latest(client: &Client, name: &str) -> anyhow::Result<String> {
    #[derive(Deserialize)]
    struct Entry {
        vers: String,
        yanked: bool,
    }

    let prefix = get_index_prefix(name).ok_or_else(|| anyhow!("invalid crate name: {name}"))?;
    let url = format!("https://index.crates.io/{}/{name}", prefix.display());
    let body = client
        .get(&url)
        .send()
        .with_context(|| format!("failed to fetch index for {name}"))?
        .text()
        .with_context(|| format!("failed to read index response for {name}"))?;

    body.lines()
        .filter_map(|line| serde_json::from_str::<Entry>(line).ok())
        .filter(|e| !e.yanked)
        .last()
        .map(|e| e.vers)
        .ok_or_else(|| anyhow!("no published versions found for {name}"))
}

pub fn prepare(
    client: &Client,
    mirror_path: &Path,
    spec: &str,
) -> anyhow::Result<(TempDir, PathBuf)> {
    let (name, maybe_version) = parse_spec(spec);
    let version = match maybe_version {
        Some(v) => v.to_string(),
        None => {
            println!("[-] Resolving latest version for {name}");
            resolve_latest(client, name)?
        }
    };

    let dir_crate_path =
        crate::get_crate_path(mirror_path, name, &version).ok_or_else(|| {
            anyhow!("could not compute mirror path for {name}-{version}")
        })?;
    let crate_file = dir_crate_path.join(format!("{name}-{version}.crate"));

    if !std::fs::exists(&crate_file).unwrap_or(false) {
        let url =
            format!("https://static.crates.io/crates/{name}/{name}-{version}.crate");
        println!("[-] Downloading: {url}");
        let response = client
            .get(&url)
            .send()
            .with_context(|| format!("failed to download {name}-{version}"))?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "{name}-{version} not found on crates.io (HTTP {})",
                response.status()
            ));
        }

        let bytes = response
            .bytes()
            .with_context(|| format!("failed to read bytes for {name}-{version}"))?;

        std::fs::create_dir_all(&dir_crate_path)?;
        std::fs::write(&crate_file, &bytes)?;
    }

    let tmp = tempfile::tempdir()?;
    let file = std::fs::File::open(&crate_file)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(tmp.path())?;

    let cargo_toml = tmp.path().join(format!("{name}-{version}")).join("Cargo.toml");
    if !cargo_toml.exists() {
        return Err(anyhow!("Cargo.toml not found in {name}-{version}.crate"));
    }

    println!("[-] Resolved {name}@{version} for dep resolution");
    Ok((tmp, cargo_toml))
}

#[cfg(test)]
mod tests {
    use super::parse_spec;

    #[test]
    fn parse_pinned() {
        assert_eq!(parse_spec("ripgrep@14.1.0"), ("ripgrep", Some("14.1.0")));
    }

    #[test]
    fn parse_no_version() {
        assert_eq!(parse_spec("ripgrep"), ("ripgrep", None));
    }

    #[test]
    fn parse_prerelease() {
        assert_eq!(parse_spec("foo@1.0.0-beta.1"), ("foo", Some("1.0.0-beta.1")));
    }

    #[test]
    fn parse_single_char_name() {
        assert_eq!(parse_spec("a@0.1.0"), ("a", Some("0.1.0")));
    }
}
