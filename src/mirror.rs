use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, bail};
use reqwest::Client;
use guppy::MetadataCommand;
use guppy::errors::Error::CommandError;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use rayon::prelude::*;
use reqwest::blocking::Client;
use reqwest::StatusCode;
use semver::{Version, VersionReq};
use serde::Deserialize;

use crate::build_std::prepare_build_std;
use crate::git::{clone, pull, write_config_json};
use crate::index::{CrateManifest, DependencySpec, extract_cargo_toml, find_crate_files};
use crate::{Crate, get_crate_path};

/// Download all crate files and put into spots that are expected by cargo from crates.io
fn download_and_save(
    mirror_path: &Path,
    vendors: Vec<(String, Vec<Crate>)>,
    build_std: bool,
    verbose: bool,
) -> anyhow::Result<()> {
    let total: u64 = vendors.iter().map(|(_, c)| c.len() as u64).sum();
    let mp = MultiProgress::new();
    let pb = mp.add(ProgressBar::new(total));
    if verbose {
        pb.set_draw_target(indicatif::ProgressDrawTarget::hidden());
    } else {
        pb.set_style(
            ProgressStyle::with_template("[{bar:40}] {pos}/{len} downloading crates")
                .unwrap()
                .progress_chars("=> "),
        );
    }

    vendors.into_par_iter().try_for_each(|(workspace, mut crates)| -> anyhow::Result<()> {
        let ws_pb = mp.insert_before(&pb, ProgressBar::new_spinner());
        ws_pb.set_style(ProgressStyle::with_template("  {msg}").unwrap());
        ws_pb.set_message(format!("Vendoring: {workspace}"));
        let client = Client::builder()
            .user_agent(format!("zerus/{} ({})", env!("CARGO_PKG_VERSION"), env!("CARGO_PKG_REPOSITORY")))
            .build()
            .context("failed to build HTTP client")?;
        crates.sort();

        crates.into_par_iter().try_for_each(|c| -> anyhow::Result<()> {
            let Crate { name, version } = c;
            let dir_crate_path = get_crate_path(mirror_path, &name, &version)
                .with_context(|| format!("invalid crate name: {name}"))?;
            let crate_path = dir_crate_path.join(format!("{name}-{version}.crate"));

            // check if file already exists
            if !fs::exists(&crate_path).unwrap_or(false) {
                let dl_pb = mp.insert_before(&pb, ProgressBar::new_spinner());
                dl_pb.set_style(ProgressStyle::with_template("  {msg}").unwrap());
                dl_pb.set_message(format!("{name}-{version}"));
                // download
                let url = format!("https://static.crates.io/crates/{name}/{name}-{version}.crate");
                if verbose {
                    println!("[-] Downloading: {url}");
                }
                let Ok(response) = client.get(url).send() else {
                    mp.remove(&dl_pb);
                    pb.inc(1);
                    return Ok(());
                };

                if response.status() != StatusCode::OK {
                    if build_std {
                        if verbose {
                            println!("[-] Couldn't download {name}-{version}, not hosted on crates.io (this is fine if it's rustc internal library)");
                        }
                        mp.remove(&dl_pb);
                        pb.inc(1);
                        return Ok(());
                    }
                    mp.remove(&dl_pb);
                    bail!("Couldn't download {name}-{version}, not hosted on crates.io");
                }

                let Ok(response) = response.bytes() else {
                    mp.remove(&dl_pb);
                    pb.inc(1);
                    return Ok(());
                };

                fs::create_dir_all(&dir_crate_path)
                    .with_context(|| format!("failed to create directory {}", dir_crate_path.display()))?;
                fs::write(&crate_path, response)
                    .with_context(|| format!("failed to write {}", crate_path.display()))?;
                mp.remove(&dl_pb);
            }
            pb.inc(1);
            Ok(())
        })?;
        mp.remove(&ws_pb);
        Ok(())
    })?;

    pb.finish_and_clear();
    Ok(())
}

/// # Returns
/// `Vec<Workspace, Vec<Crate>>`
fn get_deps(
    workspaces: &[String],
    build_std: &Option<String>,
) -> Option<Vec<(String, Vec<Crate>)>> {
    let mut workspace_list: Vec<String> = workspaces.to_vec();
    if let Some(version) = build_std {
        let build_std_path = prepare_build_std(version)?;
        workspace_list.push(build_std_path);
    }

    let mut ret = vec![];
    for workspace in &workspace_list {
        let mut crates = vec![];
        let mut cmd = MetadataCommand::new();
        cmd.manifest_path(workspace.clone());
        println!("[-] Running `cargo metadata` for {workspace}");
        let package_graph = match cmd.build_graph() {
            Ok(p) => p,
            Err(CommandError(e)) => {
                // most likely: "error: the manifest-path must be a path to a Cargo.toml file"
                println!("[!] Could not run `cargo metadata`: {e:}");
                return None;
            }
            Err(_) => {
                println!("[!] Could not run `cargo metadata`");
                return None;
            }
        };

        for package in package_graph.packages() {
            let c = Crate::new(package.name().to_string(), package.version().to_string());
            if !crates.contains(&c) {
                crates.push(c);
            }
        }
        ret.push((workspace.clone(), crates));
    }

    Some(ret)
}

/// A single version entry from the crates.io sparse index
#[derive(Deserialize)]
struct SparseIndexEntry {
    vers: String,
    yanked: bool,
}

/// Resolve a version requirement to the latest matching non-yanked version
/// by querying the crates.io sparse index.
fn resolve_version(client: &Client, name: &str, req: &VersionReq) -> Option<String> {
    let prefix = crate::get_index_prefix(name)?;
    let url = format!(
        "https://index.crates.io/{}/{}",
        prefix.display(),
        name.to_lowercase()
    );

    let response = client.get(&url).send().ok()?;
    if response.status() != StatusCode::OK {
        return None;
    }
    let body = response.text().ok()?;

    let mut best: Option<Version> = None;
    for line in body.lines() {
        let entry: SparseIndexEntry = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.yanked {
            continue;
        }
        let ver = match Version::parse(&entry.vers) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if req.matches(&ver) && best.as_ref().is_none_or(|b| ver > *b) {
            best = Some(ver);
        }
    }

    best.map(|v| v.to_string())
}

/// Collect all dependency (name, version_req) pairs from a CrateManifest.
fn collect_all_deps(manifest: &CrateManifest) -> Vec<(String, String)> {
    let mut deps = Vec::new();

    fn extract(map: &BTreeMap<String, DependencySpec>, out: &mut Vec<(String, String)>) {
        for (name, spec) in map {
            match spec {
                DependencySpec::Simple(version) => {
                    out.push((name.clone(), version.clone()));
                }
                DependencySpec::Detailed {
                    version,
                    path,
                    git,
                    package,
                    registry,
                    ..
                } => {
                    // Skip non-crates.io deps
                    if git.is_some() {
                        continue;
                    }
                    if registry.is_some() {
                        continue;
                    }
                    if let Some(ver) = version {
                        // Use the `package` name if it's a renamed dep
                        let crate_name = package.as_ref().unwrap_or(name);
                        out.push((crate_name.clone(), ver.clone()));
                    } else if path.is_some() {
                        // path-only dep without version, skip
                        continue;
                    }
                }
            }
        }
    }

    extract(&manifest.dependencies, &mut deps);
    extract(&manifest.dev_dependencies, &mut deps);
    extract(&manifest.build_dependencies, &mut deps);

    for target_deps in manifest.target.values() {
        extract(&target_deps.dependencies, &mut deps);
        extract(&target_deps.dev_dependencies, &mut deps);
        extract(&target_deps.build_dependencies, &mut deps);
    }

    deps
}

/// Iteratively expand the mirror by parsing all downloaded .crate files,
/// extracting their dependencies, resolving versions via the sparse index,
/// and downloading any missing crates. Repeats until no new crates are found.
fn expand_deps(mirror_path: &Path, verbose: bool) -> anyhow::Result<()> {
    let client = Client::builder()
        .user_agent(format!(
            "zerus/{} ({})",
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_PKG_REPOSITORY")
        ))
        .build()
        .context("failed to build HTTP client")?;

    let mut pass = 0;
    loop {
        pass += 1;
        let crates_path = mirror_path.join("crates");
        let crate_files = find_crate_files(&crates_path);

        // Collect all deps from all .crate files (parallel)
        let all_deps: Vec<(String, String)> = crate_files
            .par_iter()
            .flat_map(|crate_file| {
                let manifest = match extract_cargo_toml(crate_file) {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("[!] {e:#}");
                        return vec![];
                    }
                };
                collect_all_deps(&manifest)
            })
            .collect();

        // Deduplicate by (name, version_req)
        let unique_deps: HashSet<(String, String)> = all_deps.into_iter().collect();

        // Find which crates are already in the mirror
        let existing: HashSet<(String, String)> = crate_files
            .iter()
            .filter_map(|p| {
                let stem = p.file_stem()?.to_str()?;
                let idx = stem.rfind('-')?;
                let name = &stem[..idx];
                let version = &stem[idx + 1..];
                Some((name.to_string(), version.to_string()))
            })
            .collect();

        // Filter to deps we don't already have (sequential, cheap)
        let missing: Vec<(String, VersionReq)> = unique_deps
            .iter()
            .filter_map(|(name, version_req_str)| {
                let req = VersionReq::parse(version_req_str).ok()?;
                let already_have = existing.iter().any(|(n, v)| {
                    n == name && Version::parse(v).is_ok_and(|ver| req.matches(&ver))
                });
                if already_have {
                    None
                } else {
                    Some((name.clone(), req))
                }
            })
            .collect();

        // Resolve versions and download in parallel
        let new_downloads = AtomicUsize::new(0);
        let mp = MultiProgress::new();
        let pb = mp.add(ProgressBar::new(missing.len() as u64));
        if verbose {
            pb.set_draw_target(indicatif::ProgressDrawTarget::hidden());
        } else {
            pb.set_style(
                ProgressStyle::with_template(&format!("[{{bar:40}}] {{pos}}/{{len}} pass {pass}"))
                    .unwrap()
                    .progress_chars("=> "),
            );
        }

        missing.par_iter().for_each(|(name, req)| {
            let dl_pb = mp.insert_before(&pb, ProgressBar::new_spinner());
            dl_pb.set_style(ProgressStyle::with_template("  {msg}").unwrap());
            dl_pb.set_message(name.clone());

            let done = |dl_pb: ProgressBar, pb: &ProgressBar| {
                mp.remove(&dl_pb);
                pb.inc(1);
            };

            let Some(resolved_version) = resolve_version(&client, name, req) else {
                done(dl_pb, &pb);
                return;
            };

            if existing.contains(&(name.clone(), resolved_version.clone())) {
                done(dl_pb, &pb);
                return;
            }

            let Some(dir_crate_path) = get_crate_path(mirror_path, name, &resolved_version) else {
                done(dl_pb, &pb);
                return;
            };
            let crate_path = dir_crate_path.join(format!("{name}-{resolved_version}.crate"));

            if fs::exists(&crate_path).unwrap_or(false) {
                done(dl_pb, &pb);
                return;
            }

            let url =
                format!("https://static.crates.io/crates/{name}/{name}-{resolved_version}.crate");
            dl_pb.set_message(format!("{name}-{resolved_version}"));
            if verbose {
                println!("[-] [expand pass {pass}] Downloading: {url}");
            }

            let Ok(response) = client.get(&url).send() else {
                done(dl_pb, &pb);
                return;
            };
            if response.status() != StatusCode::OK {
                if verbose {
                    println!("[-] Couldn't download {name}-{resolved_version}");
                }
                done(dl_pb, &pb);
                return;
            }
            let Ok(bytes) = response.bytes() else {
                done(dl_pb, &pb);
                return;
            };

            if let Err(e) = fs::create_dir_all(&dir_crate_path) {
                eprintln!(
                    "[!] failed to create directory {}: {e}",
                    dir_crate_path.display()
                );
                done(dl_pb, &pb);
                return;
            }
            if let Err(e) = fs::write(&crate_path, bytes) {
                eprintln!("[!] failed to write {}: {e}", crate_path.display());
                done(dl_pb, &pb);
                return;
            }
            new_downloads.fetch_add(1, Ordering::Relaxed);
            done(dl_pb, &pb);
        });

        let count = new_downloads.load(Ordering::Relaxed);
        pb.finish_with_message(format!("pass {pass}: downloaded {count} new crate(s)"));
        println!("[-] [expand pass {pass}] Downloaded {count} new crate(s)");

        if count == 0 {
            break;
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn mirror(
    mirror_path: PathBuf,
    workspaces: Vec<String>,
    extra_crates: Vec<String>,
    build_std: Option<String>,
    git_index_url: Option<String>,
    git_index: bool,
    get_feature_gated: bool,
    verbose: bool,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(&mirror_path)
        .with_context(|| format!("failed to create {}", mirror_path.display()))?;
    println!("[-] Created {}", mirror_path.display());

    // Collect crates from workspaces (cargo metadata)
    let mut vendors: Vec<(String, Vec<Crate>)> = Vec::new();
    if !workspaces.is_empty() || build_std.is_some() {
        let Some(crates) = get_deps(&workspaces, &build_std) else {
            return Ok(());
        };
        vendors.extend(crates);
    }

    // Parse --crate arguments into a synthetic workspace entry
    if !extra_crates.is_empty() {
        let client = Client::new();
        let mut crates = Vec::new();
        for spec in &extra_crates {
            let (name, version) = if let Some((n, v)) = spec.split_once('@') {
                (n.to_string(), v.to_string())
            } else {
                // No version specified, resolve latest from sparse index
                match resolve_version(&client, spec, &VersionReq::STAR) {
                    Some(v) => {
                        println!("[-] Resolved {spec} to latest version {v}");
                        (spec.to_string(), v)
                    }
                    None => {
                        bail!("Could not resolve latest version for crate: {spec}");
                    }
                }
            };
            crates.push(Crate::new(name, version));
        }
        if !crates.is_empty() {
            let label = extra_crates.join(", ");
            vendors.push((label, crates));
        }
    }

    download_and_save(&mirror_path, vendors, build_std.is_some(), verbose)?;
    println!("[-] Finished downloading crates");

    if get_feature_gated || !extra_crates.is_empty() {
        println!("[-] Expanding feature-gated dependencies...");
        expand_deps(&mirror_path, verbose)?;
        println!("[-] Finished expanding dependencies");
    }

    if git_index {
        println!("[-] Syncing git index crates.io");
        let repo = mirror_path.join("crates.io-index");
        if repo.exists() {
            pull(Path::new(&repo)).context("failed to pull git index")?;
        } else {
            clone(Path::new(&repo)).context("failed to clone git index")?;
        }
        println!("[-] Done syncing git index crates.io");
    }

    if let Some(url) = git_index_url {
        let path = mirror_path.join("crates.io-index").join("config.json");
        let file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        write_config_json(&url, file).context("failed to write config.json")?;
    }

    Ok(())
}
