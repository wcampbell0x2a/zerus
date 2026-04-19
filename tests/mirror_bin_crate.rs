use std::fs;
use std::path::Path;
use std::process::Output;

use assert_cmd::Command;
use tempfile::TempDir;

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

fn crate_file_exists(mirror: &Path, name: &str, version: &str) -> bool {
    let prefix = match name.len() {
        1 => "1".to_string(),
        2 => "2".to_string(),
        3 => format!("3/{}", &name[..1]),
        _ => format!("{}/{}", &name[..2], &name[2..4]),
    };
    mirror
        .join("crates")
        .join(&prefix)
        .join(name)
        .join(version)
        .join(format!("{name}-{version}.crate"))
        .exists()
}

/// Walk `{mirror}/crates` and return all (name, version) pairs found on disk.
fn collected_crates(mirror: &Path) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let crates_root = mirror.join("crates");
    if !crates_root.exists() {
        return found;
    }
    for entry in walkdir(&crates_root) {
        if entry.extension().and_then(|e| e.to_str()) == Some("crate") {
            let stem = entry.file_stem().unwrap().to_string_lossy();
            // filename is "{name}-{version}.crate"; find the last '-' that precedes a digit
            if let Some(idx) = stem.rfind('-') {
                let name = stem[..idx].to_string();
                let version = stem[idx + 1..].to_string();
                found.push((name, version));
            }
        }
    }
    found
}

fn walkdir(root: &Path) -> impl Iterator<Item = std::path::PathBuf> {
    fn collect(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    collect(&p, out);
                } else {
                    out.push(p);
                }
            }
        }
    }
    let mut paths = Vec::new();
    collect(root, &mut paths);
    paths.into_iter()
}

// ── pinned version ────────────────────────────────────────────────────────────

#[test]
fn test_mirror_crate_pinned_version() {
    let tmp = TempDir::new().unwrap();

    let output = zerus_cmd()
        .args([
            "mirror",
            tmp.path().to_str().unwrap(),
            "--crate",
            "itoa@1.0.11",
        ])
        .output()
        .unwrap();
    assert_success(&output);

    assert!(
        crate_file_exists(tmp.path(), "itoa", "1.0.11"),
        "itoa-1.0.11.crate should be in mirror"
    );
}

#[test]
fn test_mirror_crate_pinned_correct_prefix_path() {
    let tmp = TempDir::new().unwrap();

    let output = zerus_cmd()
        .args([
            "mirror",
            tmp.path().to_str().unwrap(),
            "--crate",
            "itoa@1.0.11",
        ])
        .output()
        .unwrap();
    assert_success(&output);

    // "itoa" is 4 chars → prefix it/oa
    let expected = tmp
        .path()
        .join("crates/it/oa/itoa/1.0.11/itoa-1.0.11.crate");
    assert!(expected.exists(), "expected path: {}", expected.display());
}

// ── latest version (network) ──────────────────────────────────────────────────

#[test]
fn test_mirror_crate_latest_version_resolves() {
    let tmp = TempDir::new().unwrap();

    let output = zerus_cmd()
        .args(["mirror", tmp.path().to_str().unwrap(), "--crate", "itoa"])
        .output()
        .unwrap();
    assert_success(&output);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Resolving latest version for itoa"),
        "should print resolution message"
    );

    let found = collected_crates(tmp.path());
    let itoa_found = found.iter().any(|(name, _)| name == "itoa");
    assert!(itoa_found, "some version of itoa should be downloaded");
}

// ── transitive dependencies ───────────────────────────────────────────────────

#[test]
fn test_mirror_crate_downloads_transitive_deps() {
    let tmp = TempDir::new().unwrap();

    // serde_json 1.0.128 depends on itoa, ryu, and serde
    let output = zerus_cmd()
        .args([
            "mirror",
            tmp.path().to_str().unwrap(),
            "--crate",
            "serde_json@1.0.128",
        ])
        .output()
        .unwrap();
    assert_success(&output);

    let found = collected_crates(tmp.path());
    let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();

    assert!(
        names.contains(&"serde_json"),
        "serde_json itself must be present"
    );
    assert!(names.contains(&"serde"), "transitive dep serde must be present");
    assert!(names.contains(&"itoa"), "transitive dep itoa must be present");
    assert!(names.contains(&"ryu"), "transitive dep ryu must be present");
}

// ── idempotency ───────────────────────────────────────────────────────────────

#[test]
fn test_mirror_crate_idempotent() {
    let tmp = TempDir::new().unwrap();
    let args = ["mirror", tmp.path().to_str().unwrap(), "--crate", "itoa@1.0.11"];

    for _ in 0..2 {
        let output = zerus_cmd().args(args).output().unwrap();
        assert_success(&output);
    }

    // Exactly one .crate file for itoa
    let found = collected_crates(tmp.path());
    let itoa_count = found.iter().filter(|(n, v)| n == "itoa" && v == "1.0.11").count();
    assert_eq!(itoa_count, 1, "idempotent run must not duplicate the crate file");
}

// ── multiple --crate flags ─────────────────────────────────────────────────────

#[test]
fn test_mirror_multiple_crates() {
    let tmp = TempDir::new().unwrap();

    let output = zerus_cmd()
        .args([
            "mirror",
            tmp.path().to_str().unwrap(),
            "--crate",
            "itoa@1.0.11",
            "--crate",
            "ryu@1.0.18",
        ])
        .output()
        .unwrap();
    assert_success(&output);

    assert!(crate_file_exists(tmp.path(), "itoa", "1.0.11"));
    assert!(crate_file_exists(tmp.path(), "ryu", "1.0.18"));
}

// ── error cases ───────────────────────────────────────────────────────────────

#[test]
fn test_mirror_crate_nonexistent_version_fails() {
    let tmp = TempDir::new().unwrap();

    // A real crate but an impossible version
    zerus_cmd()
        .args([
            "mirror",
            tmp.path().to_str().unwrap(),
            "--crate",
            "itoa@99.99.99",
        ])
        .output()
        .unwrap();

    // zerus prints "[!] Failed to prepare" and returns without creating the file
    assert!(
        !crate_file_exists(tmp.path(), "itoa", "99.99.99"),
        "should not create a .crate file for a version that doesn't exist"
    );
}

#[test]
fn test_mirror_crate_nonexistent_crate_fails() {
    let tmp = TempDir::new().unwrap();

    let output = zerus_cmd()
        .args([
            "mirror",
            tmp.path().to_str().unwrap(),
            "--crate",
            "this-crate-absolutely-does-not-exist-xyz-abc-987@1.0.0",
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[!]"),
        "should print an error message for a nonexistent crate"
    );
}
