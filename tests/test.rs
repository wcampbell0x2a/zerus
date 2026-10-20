use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};
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
    let _server_handle = KillOnDrop(
        std::process::Command::new("python3")
            .args([
                "-m",
                "http.server",
                "-d",
                tmp_dir_path.to_str().unwrap(),
                &port.to_string(),
            ])
            .spawn()
            .expect("python3 server command failed to start"),
    );

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

/// Serve a mirror with `zerus serve`, and use the search API and the downloads through it
/// with cargo. A crate added by a later `update-index` must show in the search with no restart.
#[test]
fn test_serve_search_and_download() {
    let zerus = assert_cmd::cargo::cargo_bin("zerus");
    let tmp_dir = Builder::new().tempdir_in("./").unwrap();
    let tmp = tmp_dir.path();
    let staging = tmp.join("staging");
    let mirror = tmp.join("mirror");

    // Two crates with no depends, pinned so the test does not change when they release
    let project = tmp.join("project");
    write_project(
        &project,
        r#"itoa = "=1.0.11"
ryu = "=1.0.18""#,
    );
    let output = Command::new(&zerus)
        .args([
            "mirror",
            staging.to_str().unwrap(),
            project.join("Cargo.toml").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_success(&output);

    // Only `itoa` is in the mirror at first
    move_crate(&staging, &mirror, "itoa", "1.0.11");
    let port = free_port();
    let url = format!("http://127.0.0.1:{port}");
    update_index(&zerus, &mirror, &url);

    let _server = KillOnDrop(
        std::process::Command::new(&zerus)
            .args([
                "serve",
                mirror.to_str().unwrap(),
                "--bind",
                &format!("127.0.0.1:{port}"),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("zerus serve failed to start"),
    );
    wait_for_port(port);

    // A cargo home of its own, so the test does not use the cache of the user
    let consumer = tmp.join("consumer");
    write_project(&consumer, r#"itoa = "=1.0.11""#);
    std::fs::create_dir_all(consumer.join(".cargo")).unwrap();
    std::fs::write(
        consumer.join(".cargo/config.toml"),
        format!(
            r#"
[registries.zerus]
index = "sparse+{url}/crates.io-index/"

[source.zerus]
registry = "sparse+{url}/crates.io-index/"

[source.crates-io]
replace-with = "zerus"
"#
        ),
    )
    .unwrap();
    let cargo_home = tmp.join("cargo-home");
    let cargo = |args: &[&str]| {
        let output = std::process::Command::new("cargo")
            .args(args)
            .current_dir(&consumer)
            .env("CARGO_HOME", &cargo_home)
            // Color codes go around the crate name, and the asserts cannot find it
            .env("CARGO_TERM_COLOR", "never")
            .output()
            .unwrap();
        assert_success(&output);
        String::from_utf8(output.stdout).unwrap()
    };

    let found = cargo(&["search", "--registry", "zerus", "itoa"]);
    assert!(found.contains(r#"itoa = "1.0.11""#), "{found}");
    let found = cargo(&["search", "--registry", "zerus", "ryu"]);
    assert!(!found.contains("ryu"), "{found}");

    // Add `ryu` the way a user would, and do not restart the server
    move_crate(&staging, &mirror, "ryu", "1.0.18");
    update_index(&zerus, &mirror, &url);
    let found = cargo(&["search", "--registry", "zerus", "ryu"]);
    assert!(found.contains(r#"ryu = "1.0.18""#), "{found}");

    // Download through the server
    cargo(&["fetch"]);
    assert!(
        walk(&cargo_home.join("registry/cache")).any(|file| file.ends_with("itoa-1.0.11.crate")),
        "itoa was not downloaded"
    );
}

/// Write a library crate with these `[dependencies]` lines
fn write_project(dir: &Path, dependencies: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "").unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n{dependencies}\n",
            dir.file_name().unwrap().to_str().unwrap()
        ),
    )
    .unwrap();
}

/// Move the `.crate` file of a crate from one mirror to the other
fn move_crate(from: &Path, to: &Path, name: &str, version: &str) {
    let file = walk(&from.join("crates"))
        .find(|file| file.ends_with(format!("{name}-{version}.crate")))
        .unwrap_or_else(|| panic!("{name}@{version} is not in {}", from.display()));
    let relative = file.strip_prefix(from).unwrap();
    std::fs::create_dir_all(to.join(relative).parent().unwrap()).unwrap();
    std::fs::rename(&file, to.join(relative)).unwrap();
}

/// Every file under `dir`
fn walk(dir: &Path) -> impl Iterator<Item = std::path::PathBuf> {
    let mut files = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.into_iter()
}

fn update_index(zerus: &Path, mirror: &Path, url: &str) {
    let output = Command::new(zerus)
        .args(["update-index", mirror.to_str().unwrap(), "--dl-url", url])
        .output()
        .unwrap();
    assert_success(&output);
}

/// A port that no other process uses now
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn wait_for_port(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "the server did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Stops a server when the test ends, also when the test fails, so no server stays running
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
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
