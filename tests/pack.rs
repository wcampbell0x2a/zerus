//! End-to-end tests for the pack container: `unpack` on the CLI and the `serve` upload
//! endpoint. Both go through the same merge code, so both must land the crates and leave a
//! usable index behind.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Child, Command as StdCommand};

use assert_cmd::Command;
use tempfile::TempDir;

/// A minimal but real `.crate` file: a gzipped tar holding `{name}-{version}/Cargo.toml`.
/// The index build parses this Cargo.toml, so it has to be well formed.
fn write_crate_file(mirror: &Path, name: &str, version: &str) {
    let dir = mirror
        .join("crates")
        .join(index_prefix(name))
        .join(name)
        .join(version);
    fs::create_dir_all(&dir).unwrap();

    let manifest = format!(
        "[package]\nname = \"{name}\"\nversion = \"{version}\"\ndescription = \"test crate\"\n"
    );

    let mut tar = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(
        &mut header,
        format!("{name}-{version}/Cargo.toml"),
        manifest.as_bytes(),
    )
    .unwrap();
    let tar = tar.into_inner().unwrap();

    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&tar).unwrap();
    let gz = gz.finish().unwrap();

    fs::write(dir.join(format!("{name}-{version}.crate")), gz).unwrap();
}

/// Mirror of `get_index_prefix` in the binary, which tests cannot import
fn index_prefix(name: &str) -> String {
    match name.len() {
        1 => String::from("1"),
        2 => String::from("2"),
        3 => format!("3/{}", &name[0..1]),
        _ => format!("{}/{}", &name[0..2], &name[2..4]),
    }
}

fn zerus() -> Command {
    Command::new(assert_cmd::cargo::cargo_bin("zerus"))
}

/// Build a pack from a mirror that is already on disk, with no download step
fn pack_everything(mirror: &Path, output: &Path) {
    let out = zerus()
        .args([
            "pack-from-mirror",
            mirror.to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "pack-from-mirror failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn unpack_merges_crates_and_builds_a_usable_index() {
    let src = TempDir::new().unwrap();
    write_crate_file(src.path(), "serde", "1.0.210");
    write_crate_file(src.path(), "tokio", "1.40.0");

    let pack = src.path().join("transfer.zpk");
    pack_everything(src.path(), &pack);
    assert!(pack.is_file(), "pack file was not created");

    let dest = TempDir::new().unwrap();
    let out = zerus()
        .env("ZERUS_LOG_TEST", "1")
        .args([
            "unpack",
            dest.path().to_str().unwrap(),
            pack.to_str().unwrap(),
            "--dl-url",
            "http://127.0.0.1:8080",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "unpack failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Crates landed in the mirror layout.
    for (name, version) in [("serde", "1.0.210"), ("tokio", "1.40.0")] {
        let path = dest
            .path()
            .join("crates")
            .join(index_prefix(name))
            .join(name)
            .join(version)
            .join(format!("{name}-{version}.crate"));
        assert!(path.is_file(), "{} missing after unpack", path.display());
    }

    // The index was rebuilt over the merged result, so cargo can resolve them.
    let index = dest.path().join("crates.io-index");
    assert!(index.join("config.json").is_file(), "config.json missing");
    let entry = fs::read_to_string(index.join("se/rd/serde")).unwrap();
    assert!(
        entry.contains("\"vers\":\"1.0.210\""),
        "unexpected index entry: {entry}"
    );
}

#[test]
fn packing_without_an_output_path_writes_a_dated_pack() {
    let work = TempDir::new().unwrap();
    let mirror = work.path().join("mirror");
    write_crate_file(&mirror, "serde", "1.0.210");

    // The default path is relative, so it lands under the working directory.
    let out = zerus()
        .current_dir(work.path())
        .args(["pack-from-mirror", "mirror", "--no-record"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "pack failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let packs: Vec<_> = fs::read_dir(work.path().join("transfers"))
        .expect("no transfers directory")
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(packs.len(), 1, "expected one pack, found {packs:?}");
    // Named for today, e.g. 2026-09-06.zpk
    let name = &packs[0];
    assert!(name.ends_with(".zpk"), "not a pack file: {name}");
    assert_eq!(
        name.len(),
        "YYYY-MM-DD.zpk".len(),
        "unexpected name: {name}"
    );

    // A second pack the same day does not overwrite the first.
    let out = zerus()
        .current_dir(work.path())
        .args(["pack-from-mirror", "mirror", "--no-record"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let count = fs::read_dir(work.path().join("transfers")).unwrap().count();
    assert_eq!(count, 2, "the second pack overwrote the first");
}

#[test]
fn unpack_dry_run_lists_without_writing() {
    let src = TempDir::new().unwrap();
    write_crate_file(src.path(), "serde", "1.0.210");
    let pack = src.path().join("transfer.zpk");
    pack_everything(src.path(), &pack);

    let dest = TempDir::new().unwrap();
    let out = zerus()
        .env("ZERUS_LOG_TEST", "1")
        .args([
            "unpack",
            dest.path().to_str().unwrap(),
            pack.to_str().unwrap(),
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("serde@1.0.210"),
        "unexpected output: {stderr}"
    );
    assert!(
        !dest.path().join("crates").exists(),
        "--dry-run wrote to the mirror"
    );
}

#[test]
fn unpack_rejects_a_file_that_is_not_a_pack() {
    let tmp = TempDir::new().unwrap();
    let bogus = tmp.path().join("bogus.zpk");
    fs::write(&bogus, b"not a pack at all").unwrap();

    let out = zerus()
        .env("ZERUS_LOG_TEST", "1")
        .args([
            "unpack",
            tmp.path().join("mirror").to_str().unwrap(),
            bogus.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!out.status.success(), "a bogus pack should fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a zerus pack file"),
        "unexpected error: {stderr}"
    );
}

/// A `serve` process that is killed when the test ends
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start `serve` and wait for the port to answer
fn start_server(mirror: &Path, port: u16, token: Option<&str>) -> Server {
    start_server_with_manifests(mirror, port, token, None)
}

/// Start `serve`, optionally pointing it at a manifests directory
fn start_server_with_manifests(
    mirror: &Path,
    port: u16,
    token: Option<&str>,
    manifests: Option<&Path>,
) -> Server {
    let mut args = vec![
        String::from("serve"),
        mirror.to_string_lossy().into_owned(),
        String::from("--bind"),
        format!("127.0.0.1:{port}"),
    ];
    if let Some(dir) = manifests {
        args.push(String::from("--manifests"));
        args.push(dir.to_string_lossy().into_owned());
    }
    if let Some(token) = token {
        args.push(String::from("--upload-token"));
        args.push(token.to_string());
        args.push(String::from("--dl-url"));
        args.push(format!("http://127.0.0.1:{port}"));
    }

    let child = StdCommand::new(assert_cmd::cargo::cargo_bin("zerus"))
        .args(&args)
        .spawn()
        .unwrap();

    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    Server(child)
}

/// POST a pack as multipart/form-data, returning (status, body).
/// Sends the pack under its own file name, as a real client does.
fn upload(port: u16, pack: &Path, token: Option<&str>) -> (u16, String) {
    let boundary = "----zerustestboundary";
    let filename = pack.file_name().unwrap().to_string_lossy();
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"pack\"; \
             filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&fs::read(pack).unwrap());
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let client = reqwest::blocking::Client::new();
    let mut req = client
        .post(format!("http://127.0.0.1:{port}/admin/upload"))
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body);
    if let Some(token) = token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }

    let resp = req.send().unwrap();

    (resp.status().as_u16(), resp.text().unwrap())
}

#[test]
fn upload_merges_into_the_served_mirror_and_refreshes_the_index() {
    let src = TempDir::new().unwrap();
    write_crate_file(src.path(), "serde", "1.0.210");
    let pack = src.path().join("transfer.zpk");
    pack_everything(src.path(), &pack);

    // The server starts on a mirror that does not have the crate yet.
    let dest = TempDir::new().unwrap();
    fs::create_dir_all(dest.path().join("crates")).unwrap();
    let port = 18080;
    let _server = start_server(dest.path(), port, Some("s3cret"));

    let (status, body) = upload(port, &pack, Some("s3cret"));
    assert_eq!(status, 200, "upload failed: {body}");
    assert!(body.contains("serde@1.0.210"), "unexpected body: {body}");

    // The index is served from disk on each request, so the new crate resolves without a
    // restart.
    let entry = reqwest::blocking::get(format!(
        "http://127.0.0.1:{port}/crates.io-index/se/rd/serde"
    ))
    .unwrap();
    assert_eq!(entry.status(), 200);
    assert!(entry.text().unwrap().contains("1.0.210"));

    // A second upload of the same pack changes nothing.
    let (status, body) = upload(port, &pack, Some("s3cret"));
    assert_eq!(status, 200);
    assert!(body.contains("\"added\":0"), "unexpected body: {body}");
}

/// An uploaded pack carries its own manifest, so the server records the transfer instead
/// of leaving every uploaded crate un-manifested.
#[test]
fn an_upload_records_the_transfer_in_the_manifests_directory() {
    let src = TempDir::new().unwrap();
    write_crate_file(src.path(), "serde", "1.0.210");
    write_crate_file(src.path(), "tokio", "1.40.0");
    let pack = src.path().join("2026-09-06.zpk");
    pack_everything(src.path(), &pack);

    let dest = TempDir::new().unwrap();
    fs::create_dir_all(dest.path().join("crates")).unwrap();
    let manifests = TempDir::new().unwrap();
    let port = 18090;
    let _server =
        start_server_with_manifests(dest.path(), port, Some("s3cret"), Some(manifests.path()));

    let (status, body) = upload(port, &pack, Some("s3cret"));
    assert_eq!(status, 200, "upload failed: {body}");

    // The manifest is on disk, named after the pack.
    let recorded = fs::read_to_string(manifests.path().join("manifest-2026-09-06.txt"))
        .expect("upload did not record a manifest");
    assert!(
        recorded.contains("serde@1.0.210"),
        "unexpected record: {recorded}"
    );
    assert!(
        recorded.contains("tokio@1.40.0"),
        "unexpected record: {recorded}"
    );

    // The web UI lists it, and nothing is left un-manifested.
    let index = reqwest::blocking::get(format!("http://127.0.0.1:{port}/manifests"))
        .unwrap()
        .text()
        .unwrap();
    assert!(
        index.contains("manifest-2026-09-06.txt"),
        "uploaded transfer not listed: {index}"
    );

    let unmanifested = reqwest::blocking::get(format!("http://127.0.0.1:{port}/unmanifested"))
        .unwrap()
        .text()
        .unwrap();
    assert!(
        unmanifested.contains("Every crate in the mirror is in a manifest"),
        "uploaded crates left un-manifested: {unmanifested}"
    );
}

#[test]
fn an_upload_without_a_manifests_directory_still_merges() {
    let src = TempDir::new().unwrap();
    write_crate_file(src.path(), "serde", "1.0.210");
    let pack = src.path().join("transfer.zpk");
    pack_everything(src.path(), &pack);

    // No --manifests: there is nowhere to record, and that must not fail the upload.
    let dest = TempDir::new().unwrap();
    fs::create_dir_all(dest.path().join("crates")).unwrap();
    let port = 18091;
    let _server = start_server(dest.path(), port, Some("s3cret"));

    let (status, body) = upload(port, &pack, Some("s3cret"));

    assert_eq!(status, 200, "upload failed: {body}");
    assert!(
        dest.path()
            .join("crates/se/rd/serde/1.0.210/serde-1.0.210.crate")
            .is_file(),
        "crate did not land in the mirror"
    );
}

#[test]
fn unpack_records_the_transfer_beside_the_mirror() {
    let src = TempDir::new().unwrap();
    write_crate_file(src.path(), "serde", "1.0.210");
    let pack = src.path().join("2026-09-06.zpk");
    pack_everything(src.path(), &pack);

    let dest = TempDir::new().unwrap();
    let out = zerus()
        .env("ZERUS_LOG_TEST", "1")
        .args([
            "unpack",
            dest.path().to_str().unwrap(),
            pack.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "unpack failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let recorded = fs::read_to_string(dest.path().join("manifests/manifest-2026-09-06.txt"))
        .expect("unpack did not record a manifest");
    assert_eq!(recorded, "serde@1.0.210\n");
}

#[test]
fn unpack_with_no_record_leaves_no_manifest() {
    let src = TempDir::new().unwrap();
    write_crate_file(src.path(), "serde", "1.0.210");
    let pack = src.path().join("2026-09-06.zpk");
    pack_everything(src.path(), &pack);

    let dest = TempDir::new().unwrap();
    let out = zerus()
        .env("ZERUS_LOG_TEST", "1")
        .args([
            "unpack",
            dest.path().to_str().unwrap(),
            pack.to_str().unwrap(),
            "--no-record",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    assert!(
        !dest.path().join("manifests").exists(),
        "--no-record still wrote a manifest"
    );
}

#[test]
fn upload_without_a_token_is_unauthorized() {
    let src = TempDir::new().unwrap();
    write_crate_file(src.path(), "serde", "1.0.210");
    let pack = src.path().join("transfer.zpk");
    pack_everything(src.path(), &pack);

    let dest = TempDir::new().unwrap();
    fs::create_dir_all(dest.path().join("crates")).unwrap();
    let port = 18081;
    let _server = start_server(dest.path(), port, Some("s3cret"));

    let (status, _) = upload(port, &pack, None);
    assert_eq!(status, 401, "a missing token must not be accepted");

    let (status, _) = upload(port, &pack, Some("wrong"));
    assert_eq!(status, 401, "a wrong token must not be accepted");

    assert!(
        !dest
            .path()
            .join("crates/se/rd/serde/1.0.210/serde-1.0.210.crate")
            .exists(),
        "an unauthorized upload wrote to the mirror"
    );
}

/// POST a pack the way the browser form does: token as a field, asking for HTML back
fn upload_via_form(port: u16, pack: &Path, token: &str) -> (u16, String) {
    let boundary = "----zerusformboundary";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"pack\"; \
             filename=\"form.zpk\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&fs::read(pack).unwrap());
    body.extend_from_slice(
        format!(
            "\r\n--{boundary}\r\nContent-Disposition: form-data; \
             name=\"token\"\r\n\r\n{token}\r\n--{boundary}--\r\n"
        )
        .as_bytes(),
    );

    let resp = reqwest::blocking::Client::new()
        .post(format!("http://127.0.0.1:{port}/admin/upload"))
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header("Accept", "text/html")
        .body(body)
        .send()
        .unwrap();

    (resp.status().as_u16(), resp.text().unwrap())
}

#[test]
fn the_uploads_page_offers_a_form_when_uploads_are_on() {
    let dest = TempDir::new().unwrap();
    fs::create_dir_all(dest.path().join("crates")).unwrap();
    let port = 18083;
    let _server = start_server(dest.path(), port, Some("s3cret"));

    let body = reqwest::blocking::get(format!("http://127.0.0.1:{port}/uploads"))
        .unwrap()
        .text()
        .unwrap();

    assert!(
        body.contains("enctype=\"multipart/form-data\""),
        "no upload form: {body}"
    );
    assert!(body.contains("name=\"pack\""), "no pack field: {body}");
    assert!(body.contains("name=\"token\""), "no token field: {body}");
}

#[test]
fn the_home_page_is_the_search_box_with_the_sections_in_the_header() {
    let dest = TempDir::new().unwrap();
    write_crate_file(dest.path(), "serde", "1.0.210");
    let manifests = TempDir::new().unwrap();
    fs::write(manifests.path().join("2026-01-01.txt"), "serde@1.0.210\n").unwrap();

    let port = 18089;
    let _server =
        start_server_with_manifests(dest.path(), port, Some("s3cret"), Some(manifests.path()));

    let body = reqwest::blocking::get(format!("http://127.0.0.1:{port}/"))
        .unwrap()
        .text()
        .unwrap();

    // The search box is the page, so no listing table competes with it.
    assert!(body.contains("<form"), "no search form: {body}");
    assert!(!body.contains("<table"), "home page has a table: {body}");
    assert!(
        !body.contains("2026-01-01.txt"),
        "manifests still listed on the home page: {body}"
    );

    // Both sections are reachable from the header.
    let header = between(&body, "<header>", "</header>");
    assert!(
        header.contains("href=\"/manifests\""),
        "no manifests link in the header: {header}"
    );
    assert!(
        header.contains("href=\"/uploads\""),
        "no uploads link in the header: {header}"
    );
}

/// Text between two markers, for checking one region of a page
fn between<'a>(body: &'a str, start: &str, end: &str) -> &'a str {
    body.split_once(start)
        .and_then(|(_, rest)| rest.split_once(end))
        .map(|(inner, _)| inner)
        .unwrap_or_default()
}

/// The manifest index lists manifest files. An upload is server activity, not a crate
/// record, so it belongs in the header nav rather than in that table.
#[test]
fn the_manifest_index_does_not_list_uploads_as_a_manifest() {
    let dest = TempDir::new().unwrap();
    write_crate_file(dest.path(), "serde", "1.0.210");
    let manifests = TempDir::new().unwrap();
    fs::write(manifests.path().join("2026-01-01.txt"), "serde@1.0.210\n").unwrap();

    let port = 18087;
    let _server =
        start_server_with_manifests(dest.path(), port, Some("s3cret"), Some(manifests.path()));

    let body = reqwest::blocking::get(format!("http://127.0.0.1:{port}/manifests"))
        .unwrap()
        .text()
        .unwrap();

    // The manifest table holds the manifest file and the un-manifested view, and nothing
    // about uploads.
    let table = between(&body, "<table>", "</table>");
    assert!(
        table.contains("2026-01-01.txt"),
        "manifest missing: {table}"
    );
    assert!(
        table.contains("un-manifested"),
        "un-manifested missing: {table}"
    );
    assert!(
        !table.contains("uploads"),
        "uploads listed as a manifest: {table}"
    );

    // It is reachable from the header instead.
    let header = between(&body, "<header>", "</header>");
    assert!(
        header.contains("href=\"/uploads\""),
        "no uploads link in the header: {header}"
    );
}

#[test]
fn the_header_hides_the_uploads_link_when_uploads_are_off() {
    let dest = TempDir::new().unwrap();
    fs::create_dir_all(dest.path().join("crates")).unwrap();
    let port = 18088;
    let _server = start_server(dest.path(), port, None);

    let body = reqwest::blocking::get(format!("http://127.0.0.1:{port}/unmanifested"))
        .unwrap()
        .text()
        .unwrap();

    assert!(
        !body.contains("href=\"/uploads\""),
        "uploads link shown with uploads off: {body}"
    );
}

#[test]
fn the_uploads_page_hides_the_form_when_uploads_are_off() {
    let dest = TempDir::new().unwrap();
    fs::create_dir_all(dest.path().join("crates")).unwrap();
    let port = 18084;
    let _server = start_server(dest.path(), port, None);

    let body = reqwest::blocking::get(format!("http://127.0.0.1:{port}/uploads"))
        .unwrap()
        .text()
        .unwrap();

    assert!(
        !body.contains("multipart/form-data"),
        "form shown with uploads off"
    );
    assert!(
        body.contains("--upload-token"),
        "no explanation shown: {body}"
    );
}

#[test]
fn a_form_upload_lands_the_crates_and_answers_with_a_page() {
    let src = TempDir::new().unwrap();
    write_crate_file(src.path(), "serde", "1.0.210");
    let pack = src.path().join("transfer.zpk");
    pack_everything(src.path(), &pack);

    let dest = TempDir::new().unwrap();
    fs::create_dir_all(dest.path().join("crates")).unwrap();
    let port = 18085;
    let _server = start_server(dest.path(), port, Some("s3cret"));

    let (status, body) = upload_via_form(port, &pack, "s3cret");

    assert_eq!(status, 200, "form upload failed: {body}");
    assert!(body.contains("<html"), "expected a page, got: {body}");
    assert!(body.contains("Upload complete"), "unexpected page: {body}");
    assert!(body.contains("serde"), "crate not listed: {body}");
    assert!(
        dest.path()
            .join("crates/se/rd/serde/1.0.210/serde-1.0.210.crate")
            .is_file(),
        "crate did not land in the mirror"
    );
}

#[test]
fn a_form_upload_with_a_wrong_token_comes_back_as_a_page() {
    let src = TempDir::new().unwrap();
    write_crate_file(src.path(), "serde", "1.0.210");
    let pack = src.path().join("transfer.zpk");
    pack_everything(src.path(), &pack);

    let dest = TempDir::new().unwrap();
    fs::create_dir_all(dest.path().join("crates")).unwrap();
    let port = 18086;
    let _server = start_server(dest.path(), port, Some("s3cret"));

    let (status, body) = upload_via_form(port, &pack, "wrong");

    // The form gets the page back with the reason, not a bare status code.
    assert_eq!(status, 200);
    assert!(
        body.contains("Wrong or missing token"),
        "unexpected page: {body}"
    );
    assert!(
        !dest
            .path()
            .join("crates/se/rd/serde/1.0.210/serde-1.0.210.crate")
            .exists(),
        "a refused upload wrote to the mirror"
    );
}

#[test]
fn upload_is_absent_when_no_token_is_configured() {
    let src = TempDir::new().unwrap();
    write_crate_file(src.path(), "serde", "1.0.210");
    let pack = src.path().join("transfer.zpk");
    pack_everything(src.path(), &pack);

    let dest = TempDir::new().unwrap();
    fs::create_dir_all(dest.path().join("crates")).unwrap();
    let port = 18082;
    let _server = start_server(dest.path(), port, None);

    // Uploads off: the endpoint reports not-found rather than advertising itself.
    let (status, _) = upload(port, &pack, Some("s3cret"));
    assert_eq!(status, 404);
}

/// `pack` has no default manifests directory on purpose: a path relative to the current
/// directory would point at a different record from a different directory, and the next
/// pack would re-carry crates that already went across.
#[test]
fn pack_demands_a_manifests_directory() {
    let mirror = TempDir::new().unwrap();
    write_crate_file(mirror.path(), "serde", "1.0.210");

    let out = zerus()
        .args(["pack", mirror.path().to_str().unwrap(), "--crate", "serde"])
        .output()
        .unwrap();

    assert!(!out.status.success(), "pack ran without --manifests");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--manifests"),
        "error does not name the missing flag: {stderr}"
    );
}

/// `--no-record` is the way to pack without a manifests directory, for a transfer that may
/// not happen.
#[test]
fn pack_without_a_record_needs_no_manifests_directory() {
    let mirror = TempDir::new().unwrap();
    write_crate_file(mirror.path(), "serde", "1.0.210");
    let output = mirror.path().join("transfer.zpk");

    let out = zerus()
        .args([
            "pack-from-mirror",
            mirror.path().to_str().unwrap(),
            output.to_str().unwrap(),
            "--no-record",
        ])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "pack-from-mirror --no-record failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(output.is_file(), "pack file was not created");
}
