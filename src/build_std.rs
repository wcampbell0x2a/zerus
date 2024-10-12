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

    let base = format!("{sysroot}/lib/rustlib/src");
    if !fs::exists(&base).unwrap() {
        println!(
            "[!] failed to grab sysroot depends, try: `rustup +{version} component add rust-src`"
        );
        return None;
    }
    // before: https://github.com/rust-lang/rust/commit/1f3be75f56bfa7520b86eada306ad66455b4fd6e
    let old_from = format!("{sysroot}/lib/rustlib/src/rust/Cargo.lock");
    let from = format!("{sysroot}/lib/rustlib/src/rust/library/Cargo.lock");
    let from = if fs::exists(&old_from).unwrap() {
        Some(old_from)
    } else if fs::exists(&from).unwrap() {
        Some(from)
    } else {
        None
    };
    if let Some(from) = from {
        let to = format!("{sysroot}/lib/rustlib/src/rust/library/test/Cargo.lock");
        if fs::copy(from, to).is_err() {
            println!("[!] could not write");
            return None;
        }
    }

    Some(format!(
        "{sysroot}/lib/rustlib/src/rust/library/test/Cargo.toml"
    ))
}
