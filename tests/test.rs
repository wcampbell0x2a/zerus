use std::{
    fs::{File, OpenOptions},
    io::Write,
    process::Output,
};

use assert_cmd::Command;
use insta::assert_snapshot;
use tempfile::Builder;

#[test]
fn test_old_nightly_version() {
    let nightly_ver = "nightly-2024-05-19";
    let path = assert_cmd::cargo::cargo_bin("zerus");
    let mut cmd = Command::new(path);

    let tmp_dir = Builder::new().tempdir_in("./").unwrap();
    let tmp_dir_path = tmp_dir.into_path();
    let output = cmd
        .env("RUST_LOG", "none")
        .env("RAYON_NUM_THREADS", "1") // deterministic ordering
        .args([
            tmp_dir_path.to_str().unwrap(),
            "--skip-git-index",
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
    let output = std::str::from_utf8(&output.stdout).unwrap();

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
    let tmp_dir_path = tmp_dir.into_path();
    let output = cmd
        .env("RUST_LOG", "none")
        .env("RAYON_NUM_THREADS", "1") // deterministic ordering
        .args([
            tmp_dir_path.to_str().unwrap(),
            "--skip-git-index",
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
    let output = std::str::from_utf8(&output.stdout).unwrap();

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

fn test_build_std(nightly_ver: &str, tmp_dir_path: std::path::PathBuf, port: u32) {
    // run zerus again, but this time add the entire git index
    let path = assert_cmd::cargo::cargo_bin("zerus");
    let mut cmd = Command::new(path);
    let output = cmd
        .env("RUST_LOG", "none")
        .args([
            tmp_dir_path.to_str().unwrap(),
            "--git-index-url",
            &format!("http://127.0.0.1:{port}"),
            "--build-std",
            nightly_ver,
        ])
        .output()
        .unwrap();
    assert_success(&output);

    // Create a temp directory for a cargo project
    let tmp_dir_cargo = Builder::new().tempdir_in("./").unwrap();
    let tmp_dir_cargo_path = tmp_dir_cargo.into_path();

    // host the crates with a dummy python3 http server
    let mut server_handle = std::process::Command::new("python3")
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
