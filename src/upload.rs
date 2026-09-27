//! `zerus upload`: send a pack file to a running `zerus serve`.
//!
//! The same work as the `/uploads` page or a `curl` post, with the token in a header, the
//! file streamed from disk, and the server's answer turned into a plain message.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use indicatif::ProgressStyle;
use reqwest::blocking::multipart::{Form, Part};
use reqwest::StatusCode;
use tracing::{debug, info, info_span, Span};
use tracing_indicatif::span_ext::IndicatifSpanExt;

use crate::serve::{UploadRefusal, UploadResponse};

/// Path of the upload endpoint on a `zerus serve`
const UPLOAD_PATH: &str = "/admin/upload";

/// How long to wait for the server to accept the connection. There is no limit on the
/// transfer itself: a big pack, and the index rebuild after it, can take minutes.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Send `pack` to the server at `server` and return what the merge did
pub fn upload(pack: &Path, server: &str, token: &str) -> anyhow::Result<UploadResponse> {
    // Read locally first, so a wrong file fails here and not after a long transfer.
    let manifest = crate::pack::read_manifest(pack)?;
    let size = pack
        .metadata()
        .with_context(|| format!("failed to read {}", pack.display()))?
        .len();
    let endpoint = endpoint(server);
    info!(
        "sending {} ({} crate(s)) to {endpoint}",
        pack.display(),
        manifest.len()
    );

    let span = info_span!("upload");
    span.pb_set_style(
        &ProgressStyle::with_template(
            "[{bar:40}] {bytes}/{total_bytes} {bytes_per_sec} sending pack",
        )
        .unwrap()
        .progress_chars("=> "),
    );
    span.pb_set_length(size);
    let _enter = span.enter();

    let file = File::open(pack).with_context(|| format!("failed to open {}", pack.display()))?;
    let file_name = pack
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("upload.zpk"));
    let part = Part::reader_with_length(
        Progress {
            inner: file,
            span: span.clone(),
        },
        size,
    )
    .file_name(file_name)
    .mime_str("application/octet-stream")?;

    let client = reqwest::blocking::Client::builder()
        .user_agent(format!(
            "zerus/{} ({})",
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_PKG_REPOSITORY")
        ))
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(None::<Duration>)
        .build()
        .context("failed to set up the HTTP client")?;
    let response = client
        .post(&endpoint)
        .bearer_auth(token)
        .multipart(Form::new().part("pack", part))
        .send()
        .with_context(|| format!("failed to send the pack to {endpoint}"))?;

    let status = response.status();
    let body = response
        .text()
        .with_context(|| format!("failed to read the answer from {endpoint}"))?;
    if !status.is_success() {
        return Err(refusal(status, &body, &endpoint));
    }
    let summary: UploadResponse = serde_json::from_str(&body)
        .with_context(|| format!("{endpoint} did not answer as a zerus server: {body}"))?;

    for c in &summary.crates {
        debug!("added {c}");
    }
    info!(
        "added {} crate(s), skipped {} already in the mirror",
        summary.added, summary.skipped
    );

    Ok(summary)
}

/// The upload endpoint for a server address, with or without a trailing slash
fn endpoint(server: &str) -> String {
    format!("{}{UPLOAD_PATH}", server.trim_end_matches('/'))
}

/// Turn a refused upload into a message that tells the user what to do
fn refusal(status: StatusCode, body: &str, endpoint: &str) -> anyhow::Error {
    let reason = serde_json::from_str::<UploadRefusal>(body)
        .map(|r| r.error)
        .ok();

    match (status, reason) {
        (StatusCode::UNAUTHORIZED, _) => anyhow::anyhow!(
            "{endpoint} refused the token. Use the token that the server was started with"
        ),
        (StatusCode::NOT_FOUND, _) => anyhow::anyhow!(
            "{endpoint} does not accept uploads: start `zerus serve` with --upload-token, or \
             check the address"
        ),
        (StatusCode::PAYLOAD_TOO_LARGE, _) => {
            anyhow::anyhow!("{endpoint} refused the pack: it is larger than the server accepts")
        }
        (StatusCode::BAD_REQUEST, Some(reason)) => {
            anyhow::anyhow!("{endpoint} refused the pack: {reason}")
        }
        (status, _) if status.is_server_error() => {
            anyhow::anyhow!("{endpoint} failed to merge the pack ({status}); see the server log")
        }
        (status, _) => anyhow::anyhow!("{endpoint} answered {status}: {body}"),
    }
}

/// A reader that moves the progress bar as the HTTP client reads the pack
struct Progress<R> {
    inner: R,
    span: Span,
}

impl<R: Read> Read for Progress<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.span.pb_inc(n as u64);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_endpoint_is_under_the_server_address() {
        for server in ["http://mirror:8080", "http://mirror:8080/"] {
            assert_eq!(endpoint(server), "http://mirror:8080/admin/upload");
        }
    }

    #[test]
    fn a_refused_token_says_so() {
        let err = refusal(StatusCode::UNAUTHORIZED, "", "E").to_string();
        assert!(err.contains("refused the token"), "{err}");
    }

    #[test]
    fn a_missing_endpoint_names_the_flag_to_turn_it_on() {
        let err = refusal(StatusCode::NOT_FOUND, "", "E").to_string();
        assert!(err.contains("--upload-token"), "{err}");
    }

    #[test]
    fn a_refused_pack_carries_the_servers_reason() {
        let body = r#"{"error":"evil@1.0.0 is not a valid .crate file"}"#;
        let err = refusal(StatusCode::BAD_REQUEST, body, "E").to_string();
        assert!(
            err.contains("evil@1.0.0 is not a valid .crate file"),
            "{err}"
        );
    }

    #[test]
    fn a_server_failure_points_at_the_server_log() {
        let err = refusal(StatusCode::INTERNAL_SERVER_ERROR, "", "E").to_string();
        assert!(err.contains("server log"), "{err}");
    }

    #[test]
    fn an_unexpected_answer_shows_the_status_and_body() {
        let err = refusal(StatusCode::IM_A_TEAPOT, "short and stout", "E").to_string();
        assert!(
            err.contains("418") && err.contains("short and stout"),
            "{err}"
        );
    }

    #[test]
    fn a_bad_request_without_a_reason_still_shows_the_status() {
        let err = refusal(StatusCode::BAD_REQUEST, "", "E").to_string();
        assert!(err.contains("400"), "{err}");
    }
}
