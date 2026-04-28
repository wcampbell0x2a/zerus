use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
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

pub fn compute_cksum(path: &Path) -> String {
    let data = fs::read(path).expect("failed to read .crate file");
    let mut hasher = Sha256::new();
    hasher.update(&data);
    format!("{:x}", hasher.finalize())
}

pub fn extract_cargo_toml(path: &Path) -> CrateManifest {
    let file = fs::File::open(path).expect("failed to open .crate file");
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    for entry in archive.entries().expect("failed to read tar entries") {
        let mut entry = entry.expect("failed to read tar entry");
        let entry_path = entry.path().expect("failed to read entry path").into_owned();
        // Cargo.toml is at {name}-{version}/Cargo.toml
        if entry_path
            .file_name()
            .is_some_and(|f| f == "Cargo.toml")
            && entry_path.components().count() == 2
        {
            let mut contents = String::new();
            entry
                .read_to_string(&mut contents)
                .expect("failed to read Cargo.toml from archive");
            return toml::from_str(&contents).unwrap_or_else(|e| {
                panic!(
                    "failed to parse Cargo.toml from {}: {e}",
                    path.display()
                )
            });
        }
    }

    panic!("Cargo.toml not found in {}", path.display());
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

pub fn write_index_entry(index_path: &Path, entry: &IndexEntry) {
    let prefix = get_index_prefix(&entry.name).expect("invalid crate name for index prefix");
    let index_file = index_path.join(prefix).join(&entry.name);

    let mut entries: Vec<IndexEntry> = if index_file.exists() {
        let contents = fs::read_to_string(&index_file).expect("failed to read index file");
        contents
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).expect("failed to parse existing index entry"))
            .collect()
    } else {
        Vec::new()
    };

    if let Some(pos) = entries.iter().position(|e| e.vers == entry.vers) {
        entries.remove(pos);
    }

    fs::create_dir_all(index_file.parent().unwrap()).expect("failed to create index directory");

    let mut lines: Vec<String> = entries
        .iter()
        .map(|e| serde_json::to_string(e).expect("failed to serialize index entry"))
        .collect();
    lines.push(serde_json::to_string(entry).expect("failed to serialize index entry"));

    let content = lines.join("\n") + "\n";
    fs::write(&index_file, content).expect("failed to write index file");
}

pub fn update_index(index_path: &Path, crates_path: &Path, dl_url: Option<&str>) {
    fs::create_dir_all(index_path).expect("failed to create index directory");

    let crate_files = find_crate_files(crates_path);
    println!("[-] Found {} .crate files", crate_files.len());

    for crate_file in &crate_files {
        println!("[-] Processing: {}", crate_file.display());
        let cksum = compute_cksum(crate_file);
        let manifest = extract_cargo_toml(crate_file);
        let entry = manifest_to_index_entry(&manifest, cksum);
        write_index_entry(index_path, &entry);
    }

    if let Some(url) = dl_url {
        let config_path = index_path.join("config.json");
        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(config_path)
            .expect("failed to create config.json");
        crate::git::write_config_json(url, file).expect("failed to write config.json");
    } else if !index_path.join("config.json").exists() {
        eprintln!("[WARN] No config.json found in index and --dl-url not provided. The index will be unusable without it.");
    }

    println!("[-] Index updated at {}", index_path.display());
}

