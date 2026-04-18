use std::fs;
use std::net::TcpStream;
use std::path::Path;
use std::process::Output;
use std::time::Duration;

use assert_cmd::Command;
use tempfile::Builder;

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

fn wait_for_port(port: u32, timeout: Duration) {
    let start = std::time::Instant::now();
    loop {
        if TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            return;
        }
        if start.elapsed() > timeout {
            panic!("timed out waiting for port {port}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn download_crate(crates_dir: &Path, name: &str, version: &str) {
    let prefix = match name.len() {
        1 => "1".to_string(),
        2 => "2".to_string(),
        3 => format!("3/{}", &name[..1]),
        n if n >= 4 => format!("{}/{}", &name[..2], &name[2..4]),
        _ => panic!("empty crate name"),
    };

    let dest_dir = crates_dir.join(&prefix).join(name).join(version);
    fs::create_dir_all(&dest_dir).unwrap();

    let url = format!("https://static.crates.io/crates/{name}/{name}-{version}.crate");
    let dest_file = dest_dir.join(format!("{name}-{version}.crate"));

    let output = std::process::Command::new("curl")
        .args(["-sfL", "-o", dest_file.to_str().unwrap(), &url])
        .output()
        .expect("curl not found");

    assert!(
        output.status.success(),
        "failed to download {url}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        dest_file.exists(),
        "crate file not downloaded: {dest_file:?}"
    );
}

fn find_index_file(index_dir: &Path, crate_name: &str) -> bool {
    let prefix = match crate_name.len() {
        1 => "1".to_string(),
        2 => "2".to_string(),
        3 => format!("3/{}", &crate_name[..1]),
        n if n >= 4 => format!("{}/{}", &crate_name[..2], &crate_name[2..4]),
        _ => return false,
    };
    index_dir.join(prefix).join(crate_name).exists()
}

struct ServerGuard(std::process::Child);

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn test_update_index_serve_and_cargo_build_itoa() {
    let port: u32 = 18232;

    let tmp = Builder::new()
        .prefix("zerus-serve-")
        .tempdir_in("./")
        .unwrap();
    let root = tmp.path();
    let crates_dir = root.join("crates");
    let index_dir = root.join("crates.io-index");

    download_crate(&crates_dir, "itoa", "1.0.14");

    let server_url = format!("http://127.0.0.1:{port}");
    let output = zerus_cmd()
        .args([
            "update-index",
            root.to_str().unwrap(),
            "--dl-url",
            &server_url,
        ])
        .output()
        .unwrap();
    assert_success(&output);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Found 1 .crate files"),
        "expected 1 .crate file, got: {stdout}"
    );
    assert!(
        find_index_file(&index_dir, "itoa"),
        "index should have itoa"
    );
    assert!(
        index_dir.join("config.json").exists(),
        "config.json should exist"
    );

    let server = std::process::Command::new("python3")
        .args([
            "-m",
            "http.server",
            "-d",
            root.to_str().unwrap(),
            &port.to_string(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to start python3 http server");
    let _guard = ServerGuard(server);

    wait_for_port(port, Duration::from_secs(10));

    let project_dir = root.join("project");
    fs::create_dir_all(project_dir.join("src")).unwrap();

    fs::write(
        project_dir.join("Cargo.toml"),
        r#"[package]
name = "test-project"
version = "0.1.0"
edition = "2021"

[dependencies]
itoa = "=1.0.14"
"#,
    )
    .unwrap();

    fs::write(
        project_dir.join("src/main.rs"),
        r#"fn main() {
    let mut buf = itoa::Buffer::new();
    let s = buf.format(42u64);
    println!("{s}");
}
"#,
    )
    .unwrap();

    fs::create_dir_all(project_dir.join(".cargo")).unwrap();
    fs::write(
        project_dir.join(".cargo/config.toml"),
        format!(
            r#"[source.zerus-test]
registry = "sparse+http://127.0.0.1:{port}/crates.io-index/"

[source.crates-io]
replace-with = "zerus-test"
"#
        ),
    )
    .unwrap();

    let output = std::process::Command::new("cargo")
        .args(["build"])
        .current_dir(&project_dir)
        .env_remove("CARGO_HOME")
        .output()
        .unwrap();
    assert_success(&output);
}

#[test]
fn test_update_index_serve_and_cargo_build_multiple_crates() {
    let port: u32 = 18233;

    let tmp = Builder::new()
        .prefix("zerus-serve2-")
        .tempdir_in("./")
        .unwrap();
    let root = tmp.path();
    let crates_dir = root.join("crates");
    let index_dir = root.join("crates.io-index");

    download_crate(&crates_dir, "itoa", "1.0.14");
    download_crate(&crates_dir, "memchr", "2.7.4");

    let server_url = format!("http://127.0.0.1:{port}");
    let output = zerus_cmd()
        .args([
            "update-index",
            root.to_str().unwrap(),
            "--dl-url",
            &server_url,
        ])
        .output()
        .unwrap();
    assert_success(&output);

    assert!(find_index_file(&index_dir, "itoa"));
    assert!(find_index_file(&index_dir, "memchr"));

    let server = std::process::Command::new("python3")
        .args([
            "-m",
            "http.server",
            "-d",
            root.to_str().unwrap(),
            &port.to_string(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to start python3 http server");
    let _guard = ServerGuard(server);

    wait_for_port(port, Duration::from_secs(10));

    let project_dir = root.join("project");
    fs::create_dir_all(project_dir.join("src")).unwrap();

    fs::write(
        project_dir.join("Cargo.toml"),
        r#"[package]
name = "test-project2"
version = "0.1.0"
edition = "2021"

[dependencies]
itoa = "=1.0.14"
memchr = "=2.7.4"
"#,
    )
    .unwrap();

    fs::write(
        project_dir.join("src/main.rs"),
        r#"fn main() {
    let mut buf = itoa::Buffer::new();
    let s = buf.format(42u64);
    println!("{s}");
    let found = memchr::memchr(b'4', s.as_bytes());
    println!("{found:?}");
}
"#,
    )
    .unwrap();

    fs::create_dir_all(project_dir.join(".cargo")).unwrap();
    fs::write(
        project_dir.join(".cargo/config.toml"),
        format!(
            r#"[source.zerus-test]
registry = "sparse+http://127.0.0.1:{port}/crates.io-index/"

[source.crates-io]
replace-with = "zerus-test"
"#
        ),
    )
    .unwrap();

    let output = std::process::Command::new("cargo")
        .args(["build"])
        .current_dir(&project_dir)
        .env_remove("CARGO_HOME")
        .output()
        .unwrap();
    assert_success(&output);
}
