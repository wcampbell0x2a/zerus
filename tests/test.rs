use assert_cmd::Command;
use tempfile::tempdir;

#[test]
fn test_old_nightly_version() {
    let path = assert_cmd::cargo::cargo_bin("zerus");
    let mut cmd = Command::new(path);

    let tmp_dir = tempdir().unwrap();
    let output = cmd
        .env("RUST_LOG", "none")
        .args([
            tmp_dir.path().to_str().unwrap(),
            "--skip-git-index",
            "--build-std",
            "nightly-2024-05-19",
        ])
        .output()
        .unwrap();

    assert_eq!(
        std::str::from_utf8(&output.stdout).unwrap(),
        format!(
            r#"[-] Created {}
[-] Vendoring: /home/wcampbell/.rustup/toolchains/nightly-2024-05-19-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library/test/Cargo.toml
[-] Downloading: https://static.crates.io/crates/alloc/alloc-0.0.0.crate
[-] Downloading: https://static.crates.io/crates/allocator-api2/allocator-api2-0.2.18.crate
[-] Downloading: https://static.crates.io/crates/cfg-if/cfg-if-1.0.0.crate
[-] Downloading: https://static.crates.io/crates/compiler_builtins/compiler_builtins-0.1.109.crate
[-] Downloading: https://static.crates.io/crates/core/core-0.0.0.crate
[-] Downloading: https://static.crates.io/crates/getopts/getopts-0.2.21.crate
[-] Downloading: https://static.crates.io/crates/hashbrown/hashbrown-0.14.5.crate
[-] Downloading: https://static.crates.io/crates/libc/libc-0.2.153.crate
[-] Downloading: https://static.crates.io/crates/panic_abort/panic_abort-0.0.0.crate
[-] Downloading: https://static.crates.io/crates/panic_unwind/panic_unwind-0.0.0.crate
[-] Downloading: https://static.crates.io/crates/rustc-demangle/rustc-demangle-0.1.24.crate
[-] Downloading: https://static.crates.io/crates/rustc-std-workspace-alloc/rustc-std-workspace-alloc-1.0.0.crate
[-] Downloading: https://static.crates.io/crates/rustc-std-workspace-core/rustc-std-workspace-core-1.0.0.crate
[-] Downloading: https://static.crates.io/crates/rustc-std-workspace-std/rustc-std-workspace-std-1.0.1.crate
[-] Downloading: https://static.crates.io/crates/std/std-0.0.0.crate
[-] Downloading: https://static.crates.io/crates/std_detect/std_detect-0.1.5.crate
[-] Downloading: https://static.crates.io/crates/test/test-0.0.0.crate
[-] Downloading: https://static.crates.io/crates/unicode-width/unicode-width-0.1.12.crate
[-] Downloading: https://static.crates.io/crates/unwind/unwind-0.0.0.crate
[-] Finished downloading crates
"#,
            tmp_dir.path().to_str().unwrap()
        )
    );
}
