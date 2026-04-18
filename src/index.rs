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
        let entry_path = entry
            .path()
            .expect("failed to read entry path")
            .into_owned();
        // Cargo.toml is at {name}-{version}/Cargo.toml
        if entry_path.file_name().is_some_and(|f| f == "Cargo.toml")
            && entry_path.components().count() == 2
        {
            let mut contents = String::new();
            entry
                .read_to_string(&mut contents)
                .expect("failed to read Cargo.toml from archive");
            return toml::from_str(&contents).unwrap_or_else(|e| {
                panic!("failed to parse Cargo.toml from {}: {e}", path.display())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn create_test_crate(dir: &Path, name: &str, version: &str) -> PathBuf {
        let cargo_toml = format!(
            r#"[package]
name = "{name}"
version = "{version}"

[dependencies]
serde = "1.0"

[features]
default = ["serde"]
"#
        );

        let crate_path = dir.join(format!("{name}-{version}.crate"));
        let file = fs::File::create(&crate_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);

        let toml_bytes = cargo_toml.as_bytes();
        let mut header = tar::Header::new_gnu();
        header.set_size(toml_bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();

        builder
            .append_data(
                &mut header,
                format!("{name}-{version}/Cargo.toml"),
                toml_bytes,
            )
            .unwrap();
        builder.finish().unwrap();

        crate_path
    }

    #[test]
    fn test_compute_cksum() {
        let dir = tempfile::tempdir().unwrap();
        let test_file = dir.path().join("test.txt");
        let mut f = fs::File::create(&test_file).unwrap();
        f.write_all(b"hello").unwrap();

        let cksum = compute_cksum(&test_file);
        assert_eq!(
            cksum,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn test_extract_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        let crate_path = create_test_crate(dir.path(), "mycrate", "0.1.0");
        let manifest = extract_cargo_toml(&crate_path);

        assert_eq!(manifest.package.name, "mycrate");
        assert_eq!(manifest.package.version, "0.1.0");
        assert!(manifest.dependencies.contains_key("serde"));
        assert!(manifest.features.contains_key("default"));
    }

    #[test]
    fn test_manifest_to_index_entry() {
        let dir = tempfile::tempdir().unwrap();
        let crate_path = create_test_crate(dir.path(), "mycrate", "0.1.0");
        let manifest = extract_cargo_toml(&crate_path);
        let cksum = compute_cksum(&crate_path);
        let entry = manifest_to_index_entry(&manifest, cksum.clone());

        assert_eq!(entry.name, "mycrate");
        assert_eq!(entry.vers, "0.1.0");
        assert_eq!(entry.cksum, cksum);
        assert!(!entry.yanked);
        assert_eq!(entry.deps.len(), 1);
        assert_eq!(entry.deps[0].name, "serde");
        assert_eq!(entry.deps[0].req, "1.0");
        assert_eq!(entry.deps[0].kind, "normal");
    }

    #[test]
    fn test_write_index_entry_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let index_path = dir.path().join("index");

        let entry = IndexEntry {
            name: "serde".to_string(),
            vers: "1.0.0".to_string(),
            deps: vec![],
            cksum: "abc123".to_string(),
            features: BTreeMap::new(),
            yanked: false,
            links: None,
            v: Some(2),
        };
        write_index_entry(&index_path, &entry);

        // "serde" has 5 chars -> prefix is "se/rd"
        let index_file = index_path.join("se").join("rd").join("serde");
        assert!(index_file.exists());

        let contents = fs::read_to_string(&index_file).unwrap();
        let parsed: IndexEntry = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(parsed.name, "serde");
        assert_eq!(parsed.vers, "1.0.0");
    }

    #[test]
    fn test_write_index_entry_merges_versions() {
        let dir = tempfile::tempdir().unwrap();
        let index_path = dir.path().join("index");

        let entry1 = IndexEntry {
            name: "serde".to_string(),
            vers: "1.0.0".to_string(),
            deps: vec![],
            cksum: "abc".to_string(),
            features: BTreeMap::new(),
            yanked: false,
            links: None,
            v: Some(2),
        };
        write_index_entry(&index_path, &entry1);

        let entry2 = IndexEntry {
            name: "serde".to_string(),
            vers: "1.1.0".to_string(),
            deps: vec![],
            cksum: "def".to_string(),
            features: BTreeMap::new(),
            yanked: false,
            links: None,
            v: Some(2),
        };
        write_index_entry(&index_path, &entry2);

        let index_file = index_path.join("se").join("rd").join("serde");
        let contents = fs::read_to_string(&index_file).unwrap();
        let lines: Vec<&str> = contents.trim().lines().collect();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn test_update_index_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let crates_dir = dir.path().join("crates");
        fs::create_dir_all(&crates_dir).unwrap();

        create_test_crate(&crates_dir, "mycrate", "0.1.0");
        create_test_crate(&crates_dir, "mycrate", "0.2.0");

        let index_dir = dir.path().join("index");
        update_index(&index_dir, &crates_dir, Some("http://localhost:8080"));

        // Check index file exists
        let index_file = index_dir.join("my").join("cr").join("mycrate");
        assert!(index_file.exists());

        let contents = fs::read_to_string(&index_file).unwrap();
        let lines: Vec<&str> = contents.trim().lines().collect();
        assert_eq!(lines.len(), 2);

        // Check config.json
        let config = fs::read_to_string(index_dir.join("config.json")).unwrap();
        assert!(config.contains("localhost:8080"));
    }

    #[test]
    fn test_find_crate_files() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir_all(&sub).unwrap();

        fs::write(dir.path().join("a.crate"), b"").unwrap();
        fs::write(sub.join("b.crate"), b"").unwrap();
        fs::write(dir.path().join("c.txt"), b"").unwrap();

        let files = find_crate_files(dir.path());
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|f| f.extension().unwrap() == "crate"));
    }

    #[test]
    fn test_index_prefix_lengths() {
        assert_eq!(get_index_prefix("a"), Some(PathBuf::from("1")));
        assert_eq!(get_index_prefix("ab"), Some(PathBuf::from("2")));
        assert_eq!(get_index_prefix("abc"), Some(PathBuf::from("3").join("a")));
        assert_eq!(
            get_index_prefix("abcd"),
            Some(PathBuf::from("ab").join("cd"))
        );
        assert_eq!(
            get_index_prefix("serde"),
            Some(PathBuf::from("se").join("rd"))
        );
    }
}
