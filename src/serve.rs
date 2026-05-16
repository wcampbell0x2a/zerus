use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Json;
use serde::Serialize;

use crate::index::IndexEntry;

struct AppState {
    mirror_path: PathBuf,
}

#[derive(Serialize)]
struct SearchResponse {
    crates: Vec<SearchCrate>,
    meta: SearchMeta,
}

#[derive(Serialize)]
struct SearchCrate {
    name: String,
    max_version: String,
    description: String,
}

#[derive(Serialize)]
struct SearchMeta {
    total: usize,
}

#[derive(serde::Deserialize)]
struct SearchParams {
    q: Option<String>,
    per_page: Option<usize>,
}

fn scan_index(index_path: &Path) -> HashMap<String, String> {
    let mut crates: HashMap<String, String> = HashMap::new();
    scan_index_recursive(index_path, &mut crates);
    crates
}

fn scan_index_recursive(dir: &Path, crates: &mut HashMap<String, String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        // Skip config.json and hidden files
        if name == "config.json" || name.starts_with('.') {
            continue;
        }

        if path.is_dir() {
            scan_index_recursive(&path, crates);
        } else {
            // Each file is a crate index file with one JSON entry per line
            let contents = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for line in contents.lines() {
                if line.is_empty() {
                    continue;
                }
                let entry: IndexEntry = match serde_json::from_str(line) {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                let version = semver::Version::parse(&entry.vers).ok();
                let update = match crates.get(&entry.name) {
                    Some(existing) => {
                        let existing_ver = semver::Version::parse(existing).ok();
                        match (version.as_ref(), existing_ver.as_ref()) {
                            (Some(v), Some(e)) => v > e,
                            _ => false,
                        }
                    }
                    None => true,
                };
                if update {
                    crates.insert(entry.name.clone(), entry.vers);
                }
            }
        }
    }
}

async fn search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchParams>,
) -> impl IntoResponse {
    let query = params.q.unwrap_or_default().to_lowercase();
    let per_page = params.per_page.unwrap_or(10).min(100);

    let index_path = state.mirror_path.join("crates.io-index");
    let crates = scan_index(&index_path);

    let mut matches: Vec<(&String, &String)> = crates
        .iter()
        .filter(|(name, _)| query.is_empty() || name.to_lowercase().contains(&query))
        .collect();

    // Sort: exact match first, then starts-with, then alphabetical
    matches.sort_by(|(a, _), (b, _)| {
        let a_lower = a.to_lowercase();
        let b_lower = b.to_lowercase();
        let a_exact = a_lower == query;
        let b_exact = b_lower == query;
        let a_starts = a_lower.starts_with(&query);
        let b_starts = b_lower.starts_with(&query);
        b_exact
            .cmp(&a_exact)
            .then(b_starts.cmp(&a_starts))
            .then(a_lower.cmp(&b_lower))
    });

    let total = matches.len();
    let results: Vec<SearchCrate> = matches
        .into_iter()
        .take(per_page)
        .map(|(name, version)| SearchCrate {
            name: name.clone(),
            max_version: version.clone(),
            description: String::new(),
        })
        .collect();

    Json(SearchResponse {
        crates: results,
        meta: SearchMeta { total },
    })
}

async fn serve_crate_file(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(path): axum::extract::Path<String>,
) -> impl IntoResponse {
    let file_path = state.mirror_path.join("crates").join(&path);
    match std::fs::read(&file_path) {
        Ok(bytes) => Ok(bytes),
        Err(_) => Err(StatusCode::NOT_FOUND),
    }
}

async fn serve_index_file(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(path): axum::extract::Path<String>,
) -> impl IntoResponse {
    let file_path = state.mirror_path.join("crates.io-index").join(&path);
    match std::fs::read(&file_path) {
        Ok(bytes) => Ok(bytes),
        Err(_) => Err(StatusCode::NOT_FOUND),
    }
}

pub fn serve(mirror_path: PathBuf, bind: String) -> anyhow::Result<()> {
    let state = Arc::new(AppState { mirror_path });

    let app = axum::Router::new()
        .route("/api/v1/crates", get(search))
        .route("/crates/{*path}", get(serve_crate_file))
        .route("/crates.io-index/{*path}", get(serve_index_file))
        .with_state(state);

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind(&bind).await?;
        println!("[-] Serving on http://{bind}");
        axum::serve(listener, app).await?;
        Ok::<_, anyhow::Error>(())
    })?;

    Ok(())
}
