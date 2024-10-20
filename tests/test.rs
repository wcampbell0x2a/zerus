use std::{
    fs::{File, OpenOptions},
    io::Write,
};

use assert_cmd::Command;
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

    let rustup_home_output = std::process::Command::new("rustup")
        .args(["show", "home"])
        .output()
        .unwrap();
    let rustup_home = std::str::from_utf8(&rustup_home_output.stdout).unwrap();
    let rustup_home = rustup_home.to_string().replace("\n", "");
    assert_eq!(
        std::str::from_utf8(&output.stdout).unwrap(),
        format!(
            r#"[-] Created {}
[-] Vendoring: {rustup_home}/toolchains/{nightly_ver}-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library/test/Cargo.toml
[-] Downloading: https://static.crates.io/crates/alloc/alloc-0.0.0.crate
[-] Couldn't download alloc-0.0.0, not hosted on crates.io
[-] Downloading: https://static.crates.io/crates/allocator-api2/allocator-api2-0.2.18.crate
[-] Downloading: https://static.crates.io/crates/cfg-if/cfg-if-1.0.0.crate
[-] Downloading: https://static.crates.io/crates/compiler_builtins/compiler_builtins-0.1.109.crate
[-] Downloading: https://static.crates.io/crates/core/core-0.0.0.crate
[-] Couldn't download core-0.0.0, not hosted on crates.io
[-] Downloading: https://static.crates.io/crates/getopts/getopts-0.2.21.crate
[-] Downloading: https://static.crates.io/crates/hashbrown/hashbrown-0.14.5.crate
[-] Downloading: https://static.crates.io/crates/libc/libc-0.2.153.crate
[-] Downloading: https://static.crates.io/crates/panic_abort/panic_abort-0.0.0.crate
[-] Couldn't download panic_abort-0.0.0, not hosted on crates.io
[-] Downloading: https://static.crates.io/crates/panic_unwind/panic_unwind-0.0.0.crate
[-] Downloading: https://static.crates.io/crates/rustc-demangle/rustc-demangle-0.1.24.crate
[-] Downloading: https://static.crates.io/crates/rustc-std-workspace-alloc/rustc-std-workspace-alloc-1.0.0.crate
[-] Downloading: https://static.crates.io/crates/rustc-std-workspace-core/rustc-std-workspace-core-1.0.0.crate
[-] Downloading: https://static.crates.io/crates/rustc-std-workspace-std/rustc-std-workspace-std-1.0.1.crate
[-] Downloading: https://static.crates.io/crates/std/std-0.0.0.crate
[-] Couldn't download std-0.0.0, not hosted on crates.io
[-] Downloading: https://static.crates.io/crates/std_detect/std_detect-0.1.5.crate
[-] Downloading: https://static.crates.io/crates/test/test-0.0.0.crate
[-] Couldn't download test-0.0.0, not hosted on crates.io
[-] Downloading: https://static.crates.io/crates/unicode-width/unicode-width-0.1.12.crate
[-] Downloading: https://static.crates.io/crates/unwind/unwind-0.0.0.crate
[-] Couldn't download unwind-0.0.0, not hosted on crates.io
[-] Finished downloading crates
"#,
            tmp_dir_path.to_str().unwrap()
        )
    );

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

    let rustup_home_output = std::process::Command::new("rustup")
        .args(["show", "home"])
        .output()
        .unwrap();
    let rustup_home = std::str::from_utf8(&rustup_home_output.stdout).unwrap();
    let rustup_home = rustup_home.to_string().replace("\n", "");
    assert_eq!(
        std::str::from_utf8(&output.stdout).unwrap(),
        format!(
            r#"[-] Created {}
[-] Vendoring: {rustup_home}/toolchains/{nightly_ver}-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library/test/Cargo.toml
[-] Downloading: https://static.crates.io/crates/addr2line/addr2line-0.22.0.crate
[-] Downloading: https://static.crates.io/crates/adler/adler-1.0.2.crate
[-] Downloading: https://static.crates.io/crates/alloc/alloc-0.0.0.crate
[-] Couldn't download alloc-0.0.0, not hosted on crates.io
[-] Downloading: https://static.crates.io/crates/allocator-api2/allocator-api2-0.2.18.crate
[-] Downloading: https://static.crates.io/crates/cc/cc-1.1.22.crate
[-] Downloading: https://static.crates.io/crates/cfg-if/cfg-if-1.0.0.crate
[-] Downloading: https://static.crates.io/crates/compiler_builtins/compiler_builtins-0.1.133.crate
[-] Downloading: https://static.crates.io/crates/core/core-0.0.0.crate
[-] Couldn't download core-0.0.0, not hosted on crates.io
[-] Downloading: https://static.crates.io/crates/getopts/getopts-0.2.21.crate
[-] Downloading: https://static.crates.io/crates/gimli/gimli-0.29.0.crate
[-] Downloading: https://static.crates.io/crates/hashbrown/hashbrown-0.15.0.crate
[-] Downloading: https://static.crates.io/crates/libc/libc-0.2.159.crate
[-] Downloading: https://static.crates.io/crates/memchr/memchr-2.5.0.crate
[-] Downloading: https://static.crates.io/crates/miniz_oxide/miniz_oxide-0.7.4.crate
[-] Downloading: https://static.crates.io/crates/object/object-0.36.4.crate
[-] Downloading: https://static.crates.io/crates/panic_abort/panic_abort-0.0.0.crate
[-] Couldn't download panic_abort-0.0.0, not hosted on crates.io
[-] Downloading: https://static.crates.io/crates/panic_unwind/panic_unwind-0.0.0.crate
[-] Downloading: https://static.crates.io/crates/proc_macro/proc_macro-0.0.0.crate
[-] Couldn't download proc_macro-0.0.0, not hosted on crates.io
[-] Downloading: https://static.crates.io/crates/profiler_builtins/profiler_builtins-0.0.0.crate
[-] Couldn't download profiler_builtins-0.0.0, not hosted on crates.io
[-] Downloading: https://static.crates.io/crates/rand/rand-0.8.5.crate
[-] Downloading: https://static.crates.io/crates/rand_core/rand_core-0.6.4.crate
[-] Downloading: https://static.crates.io/crates/rand_xorshift/rand_xorshift-0.3.0.crate
[-] Downloading: https://static.crates.io/crates/rustc-demangle/rustc-demangle-0.1.24.crate
[-] Downloading: https://static.crates.io/crates/rustc-std-workspace-alloc/rustc-std-workspace-alloc-1.99.0.crate
[-] Couldn't download rustc-std-workspace-alloc-1.99.0, not hosted on crates.io
[-] Downloading: https://static.crates.io/crates/rustc-std-workspace-core/rustc-std-workspace-core-1.99.0.crate
[-] Couldn't download rustc-std-workspace-core-1.99.0, not hosted on crates.io
[-] Downloading: https://static.crates.io/crates/rustc-std-workspace-std/rustc-std-workspace-std-1.99.0.crate
[-] Couldn't download rustc-std-workspace-std-1.99.0, not hosted on crates.io
[-] Downloading: https://static.crates.io/crates/shlex/shlex-1.3.0.crate
[-] Downloading: https://static.crates.io/crates/std/std-0.0.0.crate
[-] Couldn't download std-0.0.0, not hosted on crates.io
[-] Downloading: https://static.crates.io/crates/std_detect/std_detect-0.1.5.crate
[-] Downloading: https://static.crates.io/crates/sysroot/sysroot-0.0.0.crate
[-] Couldn't download sysroot-0.0.0, not hosted on crates.io
[-] Downloading: https://static.crates.io/crates/test/test-0.0.0.crate
[-] Couldn't download test-0.0.0, not hosted on crates.io
[-] Downloading: https://static.crates.io/crates/unicode-width/unicode-width-0.1.14.crate
[-] Downloading: https://static.crates.io/crates/unwind/unwind-0.0.0.crate
[-] Couldn't download unwind-0.0.0, not hosted on crates.io
[-] Finished downloading crates
"#,
            tmp_dir_path.to_str().unwrap()
        )
    );

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
            // "--skip-git-index",
            "--build-std",
            nightly_ver,
        ])
        .output()
        .unwrap();

    // modify the config.json
    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(tmp_dir_path.join("crates.io-index/config.json"))
        .unwrap();
    file.write_all(
        &format!(
            r#"{{
  "dl": "http://127.0.0.1:{port}/crates/{{prefix}}/{{crate}}/{{version}}/{{crate}}-{{version}}.crate",
  "api": "http://127.0.0.1:{port}/crates"
}}"#
        )
        .into_bytes(),
    )
    .unwrap();

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
        .args(["new", "testing"])
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
    assert!(output.status.success());
}
