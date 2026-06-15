use std::{fs::File, io::Write, process::Output};

use assert_cmd::Command;
use insta::assert_snapshot;
use tempfile::Builder;

#[test]
fn test_old_nightly_version() {
    let nightly_ver = "nightly-2024-05-19";
    let path = assert_cmd::cargo::cargo_bin("zerus");
    let mut cmd = Command::new(path);

    let tmp_dir = Builder::new().tempdir_in("./").unwrap();
    let tmp_dir_path = tmp_dir.keep();
    let output = cmd
        .env("ZERUS_LOG_TEST", "1") // deterministic log output (no timestamps/ANSI)
        .env("RAYON_NUM_THREADS", "1") // deterministic ordering
        .args([
            "--verbose",
            "mirror",
            tmp_dir_path.to_str().unwrap(),
            "--build-std",
            nightly_ver,
        ])
        .output()
        .unwrap();
    assert_success(&output);

    let rustup_home_output = std::process::Command::new("rustup")
        .args(["show", "home"])
        .output()
        .unwrap();
    let rustup_home = std::str::from_utf8(&rustup_home_output.stdout).unwrap();
    let rustup_home = rustup_home.to_string().replace("\n", "");
    // The full verbose activity log now goes through `tracing` to stderr.
    // `ZERUS_LOG_TEST=1` strips timestamps/ANSI so this is deterministic.
    let output = std::str::from_utf8(&output.stderr).unwrap().to_string();

    // replace Create <TMP_DIR>
    let tmp_dir = tmp_dir_path.to_str().unwrap();
    let output = output.replace(&tmp_dir, "<TMP_DIR>");

    // replace RUSTUP_HOME
    let output = output.replace(&rustup_home, "<RUSTUP_HOME>");

    // replace NIGHTLY_VER
    let output = output.replace(&nightly_ver, "<NIGHTLY_VER>");

    assert_snapshot!(output);

    test_build_std(nightly_ver, tmp_dir_path.to_path_buf(), 8081);
}

#[test]
fn test_new_nightly_version() {
    let nightly_ver = "nightly-2024-10-09";
    let path = assert_cmd::cargo::cargo_bin("zerus");
    let mut cmd = Command::new(path);

    let tmp_dir = Builder::new().tempdir_in("./").unwrap();
    let tmp_dir_path = tmp_dir.keep();
    let output = cmd
        .env("ZERUS_LOG_TEST", "1") // deterministic log output (no timestamps/ANSI)
        .env("RAYON_NUM_THREADS", "1") // deterministic ordering
        .args([
            "--verbose",
            "mirror",
            tmp_dir_path.to_str().unwrap(),
            "--build-std",
            nightly_ver,
        ])
        .output()
        .unwrap();
    assert_success(&output);

    let rustup_home_output = std::process::Command::new("rustup")
        .args(["show", "home"])
        .output()
        .unwrap();
    let rustup_home = std::str::from_utf8(&rustup_home_output.stdout).unwrap();
    let rustup_home = rustup_home.to_string().replace("\n", "");
    // The full verbose activity log now goes through `tracing` to stderr.
    // `ZERUS_LOG_TEST=1` strips timestamps/ANSI so this is deterministic.
    let output = std::str::from_utf8(&output.stderr).unwrap().to_string();

    // replace Create <TMP_DIR>
    let tmp_dir = tmp_dir_path.to_str().unwrap();
    let output = output.replace(&tmp_dir, "<TMP_DIR>");

    // replace RUSTUP_HOME
    let output = output.replace(&rustup_home, "<RUSTUP_HOME>");

    // replace NIGHTLY_VER
    let output = output.replace(&nightly_ver, "<NIGHTLY_VER>");

    assert_snapshot!(output);

    test_build_std(nightly_ver, tmp_dir_path.to_path_buf(), 8080);
}

#[test]
fn test_get_feature_gated() {
    let path = assert_cmd::cargo::cargo_bin("zerus");
    let mut cmd = Command::new(path);

    let tmp_dir = Builder::new().tempdir_in("./").unwrap();
    let tmp_dir_path = tmp_dir.keep();
    let output = cmd
        .env("RUST_LOG", "none")
        .args([
            "--verbose",
            "mirror",
            tmp_dir_path.to_str().unwrap(),
            "--crate",
            "deku@0.20.3",
            "--get-feature-gated",
        ])
        .output()
        .unwrap();
    assert_success(&output);
}

#[test]
fn test_generate_manifest_and_cull() {
    let zerus = assert_cmd::cargo::cargo_bin("zerus");

    let tmp_dir = Builder::new().tempdir_in("./").unwrap();
    let mirror = tmp_dir.path();

    // fake mirror layout: crates/{prefix}/{name}/{version}/{name}-{version}.crate
    let crates = [
        ("se/rd", "serde", "1.0.210"),
        ("to/ki", "tokio", "1.40.0"),
        ("3/s", "syn", "2.0.77"),
    ];
    for (prefix, name, version) in crates {
        let dir = mirror.join("crates").join(prefix).join(name).join(version);
        std::fs::create_dir_all(&dir).unwrap();
        File::create(dir.join(format!("{name}-{version}.crate"))).unwrap();
    }

    // generate-manifest writes sorted name@version lines
    let manifest_path = mirror.join("manifest.txt");
    let output = Command::new(&zerus)
        .args([
            "generate-manifest",
            mirror.to_str().unwrap(),
            "--output",
            manifest_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(
        std::fs::read_to_string(&manifest_path).unwrap(),
        "serde@1.0.210\nsyn@2.0.77\ntokio@1.40.0\n"
    );

    // manifest of previously transferred crates, including one not on disk
    let transferred = mirror.join("transferred.txt");
    std::fs::write(&transferred, "serde@1.0.210\nsyn@2.0.77\nanyhow@1.0.0\n").unwrap();

    let serde_crate = mirror.join("crates/se/rd/serde/1.0.210/serde-1.0.210.crate");
    let syn_crate = mirror.join("crates/3/s/syn/2.0.77/syn-2.0.77.crate");
    let tokio_crate = mirror.join("crates/to/ki/tokio/1.40.0/tokio-1.40.0.crate");

    // dry-run removes nothing
    let output = Command::new(&zerus)
        .args([
            "cull",
            mirror.to_str().unwrap(),
            transferred.to_str().unwrap(),
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert_success(&output);
    assert!(serde_crate.exists());
    assert!(syn_crate.exists());
    assert!(tokio_crate.exists());

    // cull removes listed crates, prunes empty dirs, and keeps the rest
    let output = Command::new(&zerus)
        .args([
            "cull",
            mirror.to_str().unwrap(),
            transferred.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_success(&output);
    assert!(!serde_crate.exists());
    assert!(!mirror.join("crates/se").exists());
    assert!(!syn_crate.exists());
    assert!(!mirror.join("crates/3").exists());
    assert!(tokio_crate.exists());

    // only the remaining crate shows up in a new manifest
    let output = Command::new(&zerus)
        .args(["generate-manifest", mirror.to_str().unwrap()])
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(
        std::str::from_utf8(&output.stdout).unwrap(),
        "tokio@1.40.0\n"
    );
}

fn test_build_std(nightly_ver: &str, tmp_dir_path: std::path::PathBuf, port: u32) {
    // Build our own index from the downloaded .crate files
    let path = assert_cmd::cargo::cargo_bin("zerus");
    let mut cmd = Command::new(path);
    let output = cmd
        .env("RUST_LOG", "none")
        .args([
            "update-index",
            tmp_dir_path.to_str().unwrap(),
            "--dl-url",
            &format!("http://127.0.0.1:{port}"),
        ])
        .output()
        .unwrap();
    assert_success(&output);

    // Create a temp directory for a cargo project
    let tmp_dir_cargo = Builder::new().tempdir_in("./").unwrap();
    let tmp_dir_cargo_path = tmp_dir_cargo.keep();

    // host the crates with a dummy python3 http server
    let _server_handle = std::process::Command::new("python3")
        .args([
            "-m",
            "http.server",
            "-d",
            tmp_dir_path.to_str().unwrap(),
            &port.to_string(),
        ])
        .spawn()
        .expect("python3 server command failed to start");

    // create the cargo project
    std::process::Command::new("cargo")
        .args([&format!("+{nightly_ver}"), "new", "testing"])
        .current_dir(&tmp_dir_cargo_path)
        .output()
        .unwrap();
    std::process::Command::new("mkdir")
        .args(["-p", ".cargo"])
        .current_dir(&tmp_dir_cargo_path.join("testing/"))
        .output()
        .unwrap();
    // write a config file
    // 1. static binary
    // 2. build-std
    // 3. use our crates
    let mut file = File::create(&tmp_dir_cargo_path.join("testing/.cargo/config.toml")).unwrap();
    file.write_all(
        &format!(
            r#"
[source.zerus]
registry = "sparse+http://127.0.0.1:{port}/crates.io-index/"

[source.crates-io]
replace-with = "zerus"

[build]
rustflags = [
    "-C", "panic=abort",
    "-C", "target-feature=+crt-static",
]

[unstable]
build-std = ["std", "panic_abort"]
build-std-features = ["panic_immediate_abort"]
"#,
        )
        .into_bytes(),
    )
    .unwrap();

    // Run cross to create a *-musl binary that will build -Zbuild-std
    // for a specific nightly version
    let output = std::process::Command::new("cross")
        .args([
            &format!("+{nightly_ver}"),
            "build",
            "--target",
            "x86_64-unknown-linux-musl",
        ])
        // Allow access to local python server
        .env("CROSS_CONTAINER_OPTS", "--network=host")
        .current_dir(&tmp_dir_cargo_path.join("testing/"))
        .output()
        .unwrap();
    assert_success(&output);
}

fn assert_success(output: &Output) {
    if !output.status.success() {
        let stdout = String::from_utf8(output.stdout.clone()).unwrap();
        println!("stdout: {}", stdout);
        let stderr = String::from_utf8(output.stderr.clone()).unwrap();
        println!("stderr: {}", stderr);
        panic!("not success");
    }
}
