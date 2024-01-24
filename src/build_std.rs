use std::{fs, process::Command};

/// Work around for https://github.com/rust-lang/wg-cargo-std-aware/issues/23
pub fn prepare_build_std(version: &str) -> Option<String> {
    let sysroot = Command::new("rustc")
        .arg(format!("+{version}"))
        .arg("--print=sysroot")
        .output()
        .expect("command failed to start");

    let mut sysroot = std::str::from_utf8(&sysroot.stdout).unwrap().to_string();
    sysroot.pop();

    let from = format!("{sysroot}/lib/rustlib/src/rust/Cargo.lock");
    let to = format!("{sysroot}/lib/rustlib/src/rust/library/test/Cargo.lock");

    if fs::copy(from, to).is_err() {
        println!(
            "[!] failed to grab sysroot depends, try: `rustup +{version} component add rust-src`"
        );
        return None;
    }

    Some(format!(
        "{sysroot}/lib/rustlib/src/rust/library/test/Cargo.toml"
    ))
}
