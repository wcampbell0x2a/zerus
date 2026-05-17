use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use flate2::read::GzDecoder;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tar::Archive;

use crate::get_index_prefix;

#[derive(Serialize, Deserialize, Debug)]
pub struct IndexEntry {
    pub name: String,
    pub vers: String,
    pub deps: Vec<IndexDep>,
    pub cksum: String,
    pub features: BTreeMap<String, Vec<String>>,
    pub yanked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub v: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct IndexDep {
    pub name: String,
    pub req: String,
    pub features: Vec<String>,
    pub optional: bool,
    pub default_features: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct CrateManifest {
    pub package: PackageInfo,
    #[serde(default)]
    pub dependencies: BTreeMap<String, DependencySpec>,
    #[serde(default, rename = "dev-dependencies")]
    pub dev_dependencies: BTreeMap<String, DependencySpec>,
    #[serde(default, rename = "build-dependencies")]
    pub build_dependencies: BTreeMap<String, DependencySpec>,
    #[serde(default)]
    pub features: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub target: BTreeMap<String, TargetDeps>,
}

#[derive(Deserialize, Debug)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub links: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct TargetDeps {
    #[serde(default)]
    pub dependencies: BTreeMap<String, DependencySpec>,
    #[serde(default, rename = "dev-dependencies")]
    pub dev_dependencies: BTreeMap<String, DependencySpec>,
    #[serde(default, rename = "build-dependencies")]
    pub build_dependencies: BTreeMap<String, DependencySpec>,
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
pub enum DependencySpec {
    Simple(String),
    Detailed {
        #[serde(default)]
        version: Option<String>,
        #[serde(default)]
        optional: bool,
        #[serde(default = "default_true", rename = "default-features")]
        default_features: bool,
        #[serde(default)]
        features: Vec<String>,
        #[serde(default)]
        package: Option<String>,
        #[serde(default)]
        registry: Option<String>,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        git: Option<String>,
    },
}

fn default_true() -> bool {
    true
}

pub fn find_crate_files(crates_path: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    find_crate_files_recursive(crates_path, &mut result);
    result
}

fn find_crate_files_recursive(dir: &Path, result: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            find_crate_files_recursive(&path, result);
        } else if path.extension().is_some_and(|e| e == "crate") {
            result.push(path);
        }
    }
}

pub fn compute_cksum(path: &Path) -> anyhow::Result<String> {
    let data = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn extract_cargo_toml(path: &Path) -> anyhow::Result<CrateManifest> {
    let file =
        fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    for entry in archive
        .entries()
        .with_context(|| format!("failed to read tar entries from {}", path.display()))?
    {
        let mut entry =
            entry.with_context(|| format!("failed to read tar entry from {}", path.display()))?;
        let entry_path = entry
            .path()
            .with_context(|| format!("failed to read entry path from {}", path.display()))?
            .into_owned();
        // Cargo.toml is at {name}-{version}/Cargo.toml
        if entry_path.file_name().is_some_and(|f| f == "Cargo.toml")
            && entry_path.components().count() == 2
        {
            let mut contents = String::new();
            entry
                .read_to_string(&mut contents)
                .with_context(|| format!("failed to read Cargo.toml from {}", path.display()))?;
            return toml::from_str(&contents)
                .with_context(|| format!("failed to parse Cargo.toml from {}", path.display()));
        }
    }

    bail!("Cargo.toml not found in {}", path.display())
}

pub fn manifest_to_index_entry(manifest: &CrateManifest, cksum: String) -> IndexEntry {
    let mut deps = Vec::new();

    collect_deps(&manifest.dependencies, "normal", None, &mut deps);
    collect_deps(&manifest.dev_dependencies, "dev", None, &mut deps);
    collect_deps(&manifest.build_dependencies, "build", None, &mut deps);

    for (target, target_deps) in &manifest.target {
        collect_deps(&target_deps.dependencies, "normal", Some(target), &mut deps);
        collect_deps(
            &target_deps.dev_dependencies,
            "dev",
            Some(target),
            &mut deps,
        );
        collect_deps(
            &target_deps.build_dependencies,
            "build",
            Some(target),
            &mut deps,
        );
    }

    IndexEntry {
        name: manifest.package.name.clone(),
        vers: manifest.package.version.clone(),
        deps,
        cksum,
        features: manifest.features.clone(),
        yanked: false,
        links: manifest.package.links.clone(),
        v: Some(2),
    }
}

fn collect_deps(
    deps: &BTreeMap<String, DependencySpec>,
    kind: &str,
    target: Option<&String>,
    out: &mut Vec<IndexDep>,
) {
    for (name, spec) in deps {
        let dep = match spec {
            DependencySpec::Simple(version) => IndexDep {
                name: name.clone(),
                req: version.clone(),
                features: Vec::new(),
                optional: false,
                default_features: true,
                target: target.cloned(),
                kind: kind.to_string(),
                registry: None,
                package: None,
            },
            DependencySpec::Detailed {
                version,
                optional,
                default_features,
                features,
                package,
                registry,
                path,
                git,
                ..
            } => {
                // Skip path-only and git-only dependencies (no version)
                if version.is_none() && (path.is_some() || git.is_some()) {
                    continue;
                }
                IndexDep {
                    name: name.clone(),
                    req: version.clone().unwrap_or_else(|| "*".to_string()),
                    features: features.clone(),
                    optional: *optional,
                    default_features: *default_features,
                    target: target.cloned(),
                    kind: kind.to_string(),
                    registry: registry.clone(),
                    package: package.clone(),
                }
            }
        };
        out.push(dep);
    }
}

pub fn write_index_entries(index_path: &Path, new_entries: &[IndexEntry]) -> anyhow::Result<()> {
    let first = &new_entries[0];
    let prefix = get_index_prefix(&first.name).context("invalid crate name for index prefix")?;
    let index_file = index_path.join(prefix).join(&first.name);

    let mut entries: Vec<IndexEntry> = if index_file.exists() {
        let contents = fs::read_to_string(&index_file)
            .with_context(|| format!("failed to read {}", index_file.display()))?;
        contents
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l))
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| {
                format!(
                    "failed to parse existing index entry in {}",
                    index_file.display()
                )
            })?
    } else {
        Vec::new()
    };

    for new in new_entries {
        entries.retain(|e| e.vers != new.vers);
    }

    if let Some(parent) = index_file.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create index directory {}", parent.display()))?;
    }

    let lines: Vec<String> = entries
        .iter()
        .chain(new_entries.iter())
        .map(|e| serde_json::to_string(e))
        .collect::<Result<Vec<_>, _>>()
        .context("failed to serialize index entry")?;

    let content = if lines.is_empty() {
        String::new()
    } else {
        lines.join("\n") + "\n"
    };
    fs::write(&index_file, content)
        .with_context(|| format!("failed to write {}", index_file.display()))?;

    Ok(())
}

pub fn update_index(
    index_path: &Path,
    crates_path: &Path,
    dl_url: Option<&str>,
    verbose: bool,
) -> anyhow::Result<()> {
    fs::create_dir_all(index_path).context("failed to create index directory")?;

    let crate_files = find_crate_files(crates_path);
    println!("[-] Found {} .crate files", crate_files.len());

    let pb = ProgressBar::new(crate_files.len() as u64);
    if verbose {
        pb.set_draw_target(indicatif::ProgressDrawTarget::hidden());
    } else {
        pb.set_style(
            ProgressStyle::with_template("[{bar:40}] {pos}/{len} indexing crates")
                .unwrap()
                .progress_chars("=> "),
        );
    }

    // In parallel, find all crate files and compute info
    let entries: Vec<IndexEntry> = crate_files
        .par_iter()
        .map(|crate_file| {
            if verbose {
                println!("[-] Processing: {}", crate_file.display());
            }
            let cksum = compute_cksum(crate_file)?;
            let manifest = extract_cargo_toml(crate_file)?;
            pb.inc(1);
            Ok(manifest_to_index_entry(&manifest, cksum))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    pb.finish_and_clear();

    // Sort by crate name
    let mut grouped: HashMap<String, Vec<IndexEntry>> = HashMap::new();
    for entry in entries {
        grouped.entry(entry.name.clone()).or_default().push(entry);
    }

    // Write all index entries
    for (_name, entries) in &grouped {
        write_index_entries(index_path, entries)?;
    }

    if let Some(url) = dl_url {
        let config_path = index_path.join("config.json");
        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&config_path)
            .with_context(|| format!("failed to create {}", config_path.display()))?;
        crate::git::write_config_json(url, file).context("failed to write config.json")?;
    } else if !index_path.join("config.json").exists() {
        eprintln!("[WARN] No config.json found in index and --dl-url not provided. The index will be unusable without it.");
    }

    println!("[-] Index updated at {}", index_path.display());

    Ok(())
}
