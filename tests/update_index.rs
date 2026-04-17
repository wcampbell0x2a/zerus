use std::fs;
use std::path::Path;
use std::process::Output;

use assert_cmd::Command;
use tempfile::TempDir;

fn create_test_crate(dir: &Path, name: &str, version: &str, cargo_toml: &str) {
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
}

fn create_simple_crate(dir: &Path, name: &str, version: &str) {
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
    create_test_crate(dir, name, version, &cargo_toml);
}

fn zerus_cmd() -> Command {
    Command::new(assert_cmd::cargo::cargo_bin("zerus"))
}

fn assert_success(output: &Output) {
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!("command failed\nstdout: {stdout}\nstderr: {stderr}");
    }
}

fn read_index_entries(index_path: &Path, crate_name: &str) -> Vec<serde_json::Value> {
    let prefix = match crate_name.len() {
        1 => "1".to_string(),
        2 => "2".to_string(),
        3 => format!("3/{}", &crate_name[..1]),
        n if n >= 4 => format!("{}/{}", &crate_name[..2], &crate_name[2..4]),
        _ => panic!("empty crate name"),
    };

    let file = index_path.join(prefix).join(crate_name);
    assert!(file.exists(), "index file does not exist: {}", file.display());

    let contents = fs::read_to_string(&file).unwrap();
    contents
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn test_update_index_basic() {
    let tmp = TempDir::new().unwrap();
    let mirror_dir = tmp.path();
    let crates_dir = mirror_dir.join("crates");
    let index_dir = mirror_dir.join("crates.io-index");
    fs::create_dir_all(&crates_dir).unwrap();

    create_simple_crate(&crates_dir, "mycrate", "0.1.0");

    let output = zerus_cmd()
        .args(["update-index", mirror_dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert_success(&output);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Found 1 .crate files"));
    assert!(stdout.contains("Index updated"));

    let entries = read_index_entries(&index_dir, "mycrate");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "mycrate");
    assert_eq!(entries[0]["vers"], "0.1.0");
    assert_eq!(entries[0]["yanked"], false);

    // Verify checksum is a valid hex sha256 (64 chars)
    let cksum = entries[0]["cksum"].as_str().unwrap();
    assert_eq!(cksum.len(), 64);
    assert!(cksum.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn test_update_index_multiple_versions() {
    let tmp = TempDir::new().unwrap();
    let mirror_dir = tmp.path();
    let crates_dir = mirror_dir.join("crates");
    let index_dir = mirror_dir.join("crates.io-index");
    fs::create_dir_all(&crates_dir).unwrap();

    create_simple_crate(&crates_dir, "mycrate", "0.1.0");
    create_simple_crate(&crates_dir, "mycrate", "0.2.0");

    let output = zerus_cmd()
        .args(["update-index", mirror_dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert_success(&output);

    let entries = read_index_entries(&index_dir, "mycrate");
    assert_eq!(entries.len(), 2);

    let versions: Vec<&str> = entries.iter().map(|e| e["vers"].as_str().unwrap()).collect();
    assert!(versions.contains(&"0.1.0"));
    assert!(versions.contains(&"0.2.0"));
}

#[test]
fn test_update_index_multiple_crates() {
    let tmp = TempDir::new().unwrap();
    let mirror_dir = tmp.path();
    let crates_dir = mirror_dir.join("crates");
    let index_dir = mirror_dir.join("crates.io-index");
    fs::create_dir_all(&crates_dir).unwrap();

    create_simple_crate(&crates_dir, "alpha", "1.0.0");
    create_simple_crate(&crates_dir, "beta", "2.0.0");

    let output = zerus_cmd()
        .args(["update-index", mirror_dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert_success(&output);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Found 2 .crate files"));

    let alpha_entries = read_index_entries(&index_dir, "alpha");
    assert_eq!(alpha_entries.len(), 1);
    assert_eq!(alpha_entries[0]["name"], "alpha");

    let beta_entries = read_index_entries(&index_dir, "beta");
    assert_eq!(beta_entries.len(), 1);
    assert_eq!(beta_entries[0]["name"], "beta");
}

#[test]
fn test_update_index_nested_crate_files() {
    let tmp = TempDir::new().unwrap();
    let mirror_dir = tmp.path();
    let crates_dir = mirror_dir.join("crates");
    let sub_dir = crates_dir.join("sub").join("dir");
    let index_dir = mirror_dir.join("crates.io-index");
    fs::create_dir_all(&sub_dir).unwrap();

    // Put .crate in a nested subdirectory
    create_simple_crate(&sub_dir, "nested", "0.1.0");

    let output = zerus_cmd()
        .args(["update-index", mirror_dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert_success(&output);

    let entries = read_index_entries(&index_dir, "nested");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "nested");
}

#[test]
fn test_update_index_dl_url_writes_config_json() {
    let tmp = TempDir::new().unwrap();
    let mirror_dir = tmp.path();
    let crates_dir = mirror_dir.join("crates");
    let index_dir = mirror_dir.join("crates.io-index");
    fs::create_dir_all(&crates_dir).unwrap();

    create_simple_crate(&crates_dir, "mycrate", "0.1.0");

    let output = zerus_cmd()
        .args([
            "update-index",
            mirror_dir.to_str().unwrap(),
            "--dl-url",
            "http://myserver.local",
        ])
        .output()
        .unwrap();
    assert_success(&output);

    let config_path = index_dir.join("config.json");
    assert!(config_path.exists(), "config.json should be created");

    let config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    let dl = config["dl"].as_str().unwrap();
    assert!(dl.contains("myserver.local"));
    assert!(dl.contains("{crate}"));
    assert!(dl.contains("{version}"));
}

#[test]
fn test_update_index_no_dl_url_no_config_json() {
    let tmp = TempDir::new().unwrap();
    let mirror_dir = tmp.path();
    let crates_dir = mirror_dir.join("crates");
    let index_dir = mirror_dir.join("crates.io-index");
    fs::create_dir_all(&crates_dir).unwrap();

    create_simple_crate(&crates_dir, "mycrate", "0.1.0");

    let output = zerus_cmd()
        .args(["update-index", mirror_dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert_success(&output);

    let config_path = index_dir.join("config.json");
    assert!(
        !config_path.exists(),
        "config.json should not be created without --dl-url"
    );
}

#[test]
fn test_update_index_empty_crates_dir() {
    let tmp = TempDir::new().unwrap();
    let mirror_dir = tmp.path();
    let crates_dir = mirror_dir.join("crates");
    fs::create_dir_all(&crates_dir).unwrap();

    let output = zerus_cmd()
        .args(["update-index", mirror_dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert_success(&output);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Found 0 .crate files"));
}

#[test]
fn test_update_index_deps_in_index_entry() {
    let tmp = TempDir::new().unwrap();
    let mirror_dir = tmp.path();
    let crates_dir = mirror_dir.join("crates");
    let index_dir = mirror_dir.join("crates.io-index");
    fs::create_dir_all(&crates_dir).unwrap();

    let cargo_toml = r#"[package]
name = "deptest"
version = "0.3.0"

[dependencies]
serde = "1.0"
log = "0.4"

[dev-dependencies]
tempfile = "3.0"

[build-dependencies]
cc = "1.0"

[features]
default = ["serde"]
extra = ["log"]
"#;
    create_test_crate(&crates_dir, "deptest", "0.3.0", cargo_toml);

    let output = zerus_cmd()
        .args(["update-index", mirror_dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert_success(&output);

    let entries = read_index_entries(&index_dir, "deptest");
    assert_eq!(entries.len(), 1);

    let entry = &entries[0];
    let deps = entry["deps"].as_array().unwrap();

    // Check we have all 4 deps
    let dep_names: Vec<&str> = deps.iter().map(|d| d["name"].as_str().unwrap()).collect();
    assert!(dep_names.contains(&"serde"));
    assert!(dep_names.contains(&"log"));
    assert!(dep_names.contains(&"tempfile"));
    assert!(dep_names.contains(&"cc"));

    // Check kinds
    let find_dep = |name: &str| deps.iter().find(|d| d["name"] == name).unwrap();
    assert_eq!(find_dep("serde")["kind"], "normal");
    assert_eq!(find_dep("log")["kind"], "normal");
    assert_eq!(find_dep("tempfile")["kind"], "dev");
    assert_eq!(find_dep("cc")["kind"], "build");

    // Check features
    let features = entry["features"].as_object().unwrap();
    assert!(features.contains_key("default"));
    assert!(features.contains_key("extra"));
    let default_features: Vec<&str> = features["default"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(default_features, vec!["serde"]);
}

#[test]
fn test_update_index_detailed_dependency() {
    let tmp = TempDir::new().unwrap();
    let mirror_dir = tmp.path();
    let crates_dir = mirror_dir.join("crates");
    let index_dir = mirror_dir.join("crates.io-index");
    fs::create_dir_all(&crates_dir).unwrap();

    let cargo_toml = r#"[package]
name = "detailed"
version = "1.0.0"

[dependencies]
serde = { version = "1.0", features = ["derive"], optional = true, default-features = false }
"#;
    create_test_crate(&crates_dir, "detailed", "1.0.0", cargo_toml);

    let output = zerus_cmd()
        .args(["update-index", mirror_dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert_success(&output);

    let entries = read_index_entries(&index_dir, "detailed");
    let deps = entries[0]["deps"].as_array().unwrap();
    assert_eq!(deps.len(), 1);

    let dep = &deps[0];
    assert_eq!(dep["name"], "serde");
    assert_eq!(dep["req"], "1.0");
    assert_eq!(dep["optional"], true);
    assert_eq!(dep["default_features"], false);
    let features: Vec<&str> = dep["features"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(features, vec!["derive"]);
}

#[test]
fn test_update_index_target_specific_deps() {
    let tmp = TempDir::new().unwrap();
    let mirror_dir = tmp.path();
    let crates_dir = mirror_dir.join("crates");
    let index_dir = mirror_dir.join("crates.io-index");
    fs::create_dir_all(&crates_dir).unwrap();

    let cargo_toml = r#"[package]
name = "targeted"
version = "0.1.0"

[target.'cfg(unix)'.dependencies]
nix = "0.29"

[target.'cfg(windows)'.dependencies]
winapi = "0.3"
"#;
    create_test_crate(&crates_dir, "targeted", "0.1.0", cargo_toml);

    let output = zerus_cmd()
        .args(["update-index", mirror_dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert_success(&output);

    let entries = read_index_entries(&index_dir, "targeted");
    let deps = entries[0]["deps"].as_array().unwrap();
    assert_eq!(deps.len(), 2);

    let nix_dep = deps.iter().find(|d| d["name"] == "nix").unwrap();
    assert_eq!(nix_dep["target"], "cfg(unix)");
    assert_eq!(nix_dep["kind"], "normal");

    let winapi_dep = deps.iter().find(|d| d["name"] == "winapi").unwrap();
    assert_eq!(winapi_dep["target"], "cfg(windows)");
}

#[test]
fn test_update_index_idempotent() {
    let tmp = TempDir::new().unwrap();
    let mirror_dir = tmp.path();
    let crates_dir = mirror_dir.join("crates");
    let index_dir = mirror_dir.join("crates.io-index");
    fs::create_dir_all(&crates_dir).unwrap();

    create_simple_crate(&crates_dir, "mycrate", "0.1.0");

    // Run twice
    for _ in 0..2 {
        let output = zerus_cmd()
            .args(["update-index", mirror_dir.to_str().unwrap()])
            .output()
            .unwrap();
        assert_success(&output);
    }

    // Should still have exactly one entry, not duplicated
    let entries = read_index_entries(&index_dir, "mycrate");
    assert_eq!(entries.len(), 1);
}

#[test]
fn test_update_index_prefix_paths() {
    let tmp = TempDir::new().unwrap();
    let mirror_dir = tmp.path();
    let crates_dir = mirror_dir.join("crates");
    let index_dir = mirror_dir.join("crates.io-index");
    fs::create_dir_all(&crates_dir).unwrap();

    // 1-char name -> prefix "1/"
    create_simple_crate(&crates_dir, "a", "0.1.0");
    // 2-char name -> prefix "2/"
    create_simple_crate(&crates_dir, "ab", "0.1.0");
    // 3-char name -> prefix "3/{first_char}/"
    create_simple_crate(&crates_dir, "abc", "0.1.0");
    // 4+-char name -> prefix "{first_two}/{second_two}/"
    create_simple_crate(&crates_dir, "abcd", "0.1.0");

    let output = zerus_cmd()
        .args(["update-index", mirror_dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert_success(&output);

    assert!(index_dir.join("1").join("a").exists());
    assert!(index_dir.join("2").join("ab").exists());
    assert!(index_dir.join("3").join("a").join("abc").exists());
    assert!(index_dir.join("ab").join("cd").join("abcd").exists());
}

#[test]
fn test_update_index_checksum_is_of_crate_file() {
    let tmp = TempDir::new().unwrap();
    let mirror_dir = tmp.path();
    let crates_dir = mirror_dir.join("crates");
    let index_dir = mirror_dir.join("crates.io-index");
    fs::create_dir_all(&crates_dir).unwrap();

    create_simple_crate(&crates_dir, "cktest", "0.1.0");

    let output = zerus_cmd()
        .args(["update-index", mirror_dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert_success(&output);

    let entries = read_index_entries(&index_dir, "cktest");
    let index_cksum = entries[0]["cksum"].as_str().unwrap();

    // Compute expected checksum manually
    use sha2::{Digest, Sha256};
    let crate_file = crates_dir.join("cktest-0.1.0.crate");
    let data = fs::read(&crate_file).unwrap();
    let expected = format!("{:x}", Sha256::digest(&data));

    assert_eq!(index_cksum, expected);
}

#[test]
fn test_update_index_dl_url_requires_valid_url() {
    let tmp = TempDir::new().unwrap();
    let mirror_dir = tmp.path();
    let crates_dir = mirror_dir.join("crates");
    fs::create_dir_all(&crates_dir).unwrap();

    let output = zerus_cmd()
        .args([
            "update-index",
            mirror_dir.to_str().unwrap(),
            "--dl-url",
            "not-a-url",
        ])
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "should fail with invalid --dl-url"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("http://") || stderr.contains("https://"));
}

#[test]
fn test_update_index_path_only_deps_skipped() {
    let tmp = TempDir::new().unwrap();
    let mirror_dir = tmp.path();
    let crates_dir = mirror_dir.join("crates");
    let index_dir = mirror_dir.join("crates.io-index");
    fs::create_dir_all(&crates_dir).unwrap();

    let cargo_toml = r#"[package]
name = "pathskip"
version = "0.1.0"

[dependencies]
local-dep = { path = "../local-dep" }
real-dep = "1.0"
"#;
    create_test_crate(&crates_dir, "pathskip", "0.1.0", cargo_toml);

    let output = zerus_cmd()
        .args(["update-index", mirror_dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert_success(&output);

    let entries = read_index_entries(&index_dir, "pathskip");
    let deps = entries[0]["deps"].as_array().unwrap();

    // Only real-dep should appear; local-dep (path-only, no version) should be skipped
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0]["name"], "real-dep");
}
