use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use maud::{html, Markup, PreEscaped, DOCTYPE};
use serde::{Deserialize, Serialize};
use time::macros::format_description;
use time::OffsetDateTime;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use crate::get_crate_path;
use crate::index::{extract_cargo_toml, IndexEntry};
use crate::manifest::{self, ManifestFile};
use crate::Crate;

struct AppState {
    mirror_path: PathBuf,
    /// Directory of manifest files from previous transfers, if `--manifests` was given
    manifests_path: Option<PathBuf>,
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
        .map(|(name, version)| {
            let description = get_crate_path(&state.mirror_path, name, version)
                .and_then(|dir| {
                    let crate_file = dir.join(format!("{name}-{version}.crate"));
                    extract_cargo_toml(&crate_file).ok()
                })
                .and_then(|manifest| manifest.package.description)
                .unwrap_or_default();
            SearchCrate {
                name: name.clone(),
                max_version: version.clone(),
                description,
            }
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

/// Load the manifests dir, or `None` if `--manifests` was not given
fn load_manifests(state: &AppState) -> Result<Option<Vec<ManifestFile>>, StatusCode> {
    let Some(dir) = state.manifests_path.as_deref() else {
        return Ok(None);
    };

    match manifest::load_dir(dir) {
        Ok(manifests) => Ok(Some(manifests)),
        Err(e) => {
            warn!("failed to read manifests from {}: {e:#}", dir.display());
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Modification time of the `.crate` file, or `None` if it is not in the mirror.
/// Absent means the crate was already transferred and culled.
fn written_at(mirror_path: &Path, name: &str, version: &str) -> Option<SystemTime> {
    let dir = get_crate_path(mirror_path, name, version)?;
    let file = dir.join(format!("{name}-{version}.crate"));
    file.metadata().ok()?.modified().ok()
}

/// Render a time as `YYYY-MM-DD HH:MM` UTC, the form the manifest file names use
fn format_time(time: SystemTime) -> String {
    OffsetDateTime::from(time)
        .format(format_description!("[year]-[month]-[day] [hour]:[minute]"))
        .unwrap_or_default()
}

/// Column a crate listing is ordered by
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Name,
    Written,
}

impl Sort {
    /// The `sort=` value that asks for this order
    fn param(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Written => "written",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [Self::Name, Self::Written]
            .into_iter()
            .find(|sort| sort.param() == value)
    }
}

/// A crate in a listing, paired with the write time of its `.crate` file.
/// `written` is `None` for a culled crate, which no longer has a file to stat.
struct Listed<'a> {
    krate: &'a Crate,
    written: Option<SystemTime>,
}

impl Listed<'_> {
    fn in_mirror(&self) -> bool {
        self.written.is_some()
    }
}

/// Pair each crate with its write time and order the result by `sort`.
/// Newest first when sorting by write time; culled crates have no time, so they sort last.
fn listing<'a>(mirror_path: &Path, crates: &'a [Crate], sort: Sort) -> Vec<Listed<'a>> {
    let mut listed: Vec<Listed<'a>> = crates
        .iter()
        .map(|krate| Listed {
            krate,
            written: written_at(mirror_path, &krate.name, &krate.version),
        })
        .collect();

    match sort {
        Sort::Name => listed.sort_by(|a, b| a.krate.cmp(b.krate)),
        Sort::Written => {
            listed.sort_by(|a, b| b.written.cmp(&a.written).then_with(|| a.krate.cmp(b.krate)))
        }
    }

    listed
}

/// Split a listing into the crates still in the mirror and the culled ones, keeping the
/// order `listing` put them in
fn split_culled<'a, 'b>(listed: &'b [Listed<'a>]) -> (Vec<&'b Listed<'a>>, Vec<&'b Listed<'a>>) {
    listed.iter().partition(|l| l.in_mirror())
}

#[derive(serde::Deserialize)]
struct SortParams {
    /// `None` for both a missing and an unrecognized `sort=`, so a hand-edited URL
    /// falls back to the default order instead of failing the request
    #[serde(default, deserialize_with = "ignore_unknown_sort")]
    sort: Option<Sort>,
}

fn ignore_unknown_sort<'de, D>(deserializer: D) -> Result<Option<Sort>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(raw.as_deref().and_then(Sort::parse))
}

impl SortParams {
    fn sort(&self) -> Sort {
        self.sort.unwrap_or(Sort::Name)
    }
}

/// A crate table with sortable `crate` and `written` column headers.
/// `base` is the path the header links point back at, e.g. `/manifests/2026-08-13.txt`.
fn crate_table(listed: &[&Listed<'_>], base: &str, sort: Sort) -> Markup {
    html! {
        table {
            thead {
                tr {
                    th { (sort_link(base, "crate", Sort::Name, sort)) }
                    th { "version" }
                    th { (sort_link(base, "written", Sort::Written, sort)) }
                }
            }
            tbody {
                @for l in listed {
                    tr .culled[!l.in_mirror()] {
                        td { (l.krate.name) }
                        td { (l.krate.version) }
                        td.count {
                            @match l.written {
                                Some(t) => (format_time(t)),
                                None => "-",
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The culled crates, behind a "show culled" toggle so the crates that still have to make
/// the trip stay in view. Empty markup when nothing was culled.
fn culled_section(culled: &[&Listed<'_>], base: &str, sort: Sort) -> Markup {
    html! {
        @if !culled.is_empty() {
            details.culled-group {
                summary { "show culled (" (culled.len()) ")" }
                (crate_table(culled, base, sort))
            }
        }
    }
}

/// Column header that re-requests the page sorted by `column`
fn sort_link(base: &str, label: &str, column: Sort, active: Sort) -> Markup {
    html! {
        a.sort.active[column == active] href={ (base) "?sort=" (column.param()) } { (label) }
    }
}

/// Does the search box take the cursor as the page loads?
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    /// The search page, where searching is the reason the user is there
    Search,
    /// Every other page, where the cursor must stay free for the `s` and `/` shortcuts
    Page,
}

fn page(title: &str, body: Markup) -> Markup {
    page_with_search(title, "", Focus::Page, body)
}

/// The page shell. The search box lives in the header, so it is on every page.
fn page_with_search(title: &str, query: &str, focus: Focus, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) }
                style { (PAGE_CSS) }
            }
            body {
                header {
                    a href="/" { "zerus" }
                    span.version { "v" (env!("CARGO_PKG_VERSION")) }
                    (search_form(query, focus))
                }
                main { (body) }
                script { (PreEscaped(SEARCH_SHORTCUT_JS)) }
            }
        }
    }
}

const PAGE_CSS: &str = "\
:root { color-scheme: light dark; }
/* Only fonts already on the machine. The mirror serves offline networks, so a webfont
   download would fail and drop the page back to the browser default. */
body { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, 'Cascadia Mono',
       'DejaVu Sans Mono', 'Liberation Mono', monospace; margin: 0 auto;
       max-width: 60rem; padding: 1.5rem; line-height: 1.5; }
header { border-bottom: 1px solid currentColor; margin-bottom: 1.5rem; padding-bottom: .5rem; }
header a { font-weight: bold; text-decoration: none; color: inherit; }
header .version { margin-left: .5rem; font-size: .85em;
                  color: color-mix(in srgb, currentColor 65%, transparent); }
table { border-collapse: collapse; width: 100%; }
td, th { text-align: left; padding: .25rem .75rem .25rem 0; }
th { border-bottom: 1px solid currentColor; }
tr + tr td { border-top: 1px solid color-mix(in srgb, currentColor 15%, transparent); }
.culled { opacity: .55; }
.count { color: color-mix(in srgb, currentColor 65%, transparent); }
input[type=search] { font: inherit; padding: .3rem; min-width: 16rem; }
p.empty { color: color-mix(in srgb, currentColor 65%, transparent); }
details.culled-group { margin-top: 1.5rem; }
details.culled-group summary { cursor: pointer; padding: .25rem 0;
                               color: color-mix(in srgb, currentColor 65%, transparent); }
a.sort { color: inherit; text-decoration: none; }
a.sort:hover { text-decoration: underline; }
a.sort.active::after { content: ' \\2193'; }
form.search { display: inline; margin-left: 1rem; }
";

// `s` and `/` focus the search box, the same keys docs.rs binds. Ctrl+S is left alone so
// the browser can still save the page. Escape gives the cursor back to the page.
const SEARCH_SHORTCUT_JS: &str = "\
document.addEventListener('keydown', function (e) {
  var box = document.getElementById('search');
  if (!box) return;
  if (e.key === 'Escape' && document.activeElement === box) { box.blur(); return; }
  if (e.ctrlKey || e.metaKey || e.altKey) return;
  // Ignore the keypress while the user types into a field, or it would eat the character.
  var el = document.activeElement;
  var tag = el ? el.tagName : '';
  if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || (el && el.isContentEditable)) {
    return;
  }
  if (e.key === 's' || e.key === '/') { e.preventDefault(); box.focus(); }
});
";

/// Shown when --manifests was not passed
fn no_manifests_page() -> Markup {
    page(
        "manifests - zerus",
        html! {
            h1 { "No manifests directory" }
            p {
                "Start the server with "
                code { "--manifests <DIR>" }
                " to browse the manifest files written by "
                code { "generate-manifest" }
                "."
            }
            pre { "zerus serve <mirror> --manifests transfers/" }
        },
    )
}

fn search_form(query: &str, focus: Focus) -> Markup {
    html! {
        form.search action="/search" method="get" {
            input #search type="search" name="q" value=(query)
                  placeholder="find a crate (press s)"
                  autofocus[focus == Focus::Search];
            " "
            button type="submit" { "search" }
        }
    }
}

/// Index: every manifest file with its crate count
async fn manifests_index(State(state): State<Arc<AppState>>) -> Result<Markup, StatusCode> {
    let Some(manifests) = load_manifests(&state)? else {
        return Ok(no_manifests_page());
    };

    // A crate is only in the mirror because someone put it there, so this page always
    // offers the un-manifested view, even when no manifest file exists yet.
    let unmanifested_count = manifest::unmanifested(&state.mirror_path, &manifests)
        .map(|crates| crates.len())
        .unwrap_or_default();

    Ok(page(
        "crates - zerus",
        html! {
            h1 { "Crates" }
            table {
                thead { tr { th { "manifest" } th { "crates" } } }
                tbody {
                    @for m in &manifests {
                        tr {
                            td { a href={ "/manifests/" (m.name) } { (m.name) } }
                            td.count { (m.crates.len()) }
                        }
                    }
                    tr {
                        td { a href="/unmanifested" { "(un-manifested)" } }
                        td.count { (unmanifested_count) }
                    }
                }
            }
            @if manifests.is_empty() {
                p.empty { "No manifest files found." }
            }
        },
    ))
}

/// Detail: the crates listed in one manifest, marked present or culled
async fn manifest_detail(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
    Query(params): Query<SortParams>,
) -> Result<Markup, StatusCode> {
    let Some(manifests) = load_manifests(&state)? else {
        return Ok(no_manifests_page());
    };

    // Match against the listing rather than joining user input onto a path
    let found = manifests
        .into_iter()
        .find(|m| m.name == name)
        .ok_or(StatusCode::NOT_FOUND)?;

    let listed = listing(&state.mirror_path, &found.crates, params.sort());
    let (present, culled) = split_culled(&listed);
    let base = format!("/manifests/{}", found.name);

    Ok(page(
        &format!("{} - zerus", found.name),
        html! {
            h1 { (found.name) }
            p.count {
                (listed.len()) " crate(s), " (present.len()) " still in mirror, "
                (culled.len()) " culled"
            }
            @if present.is_empty() {
                p.empty { "Every crate in this manifest was culled." }
            } @else {
                (crate_table(&present, &base, params.sort()))
            }
            (culled_section(&culled, &base, params.sort()))
        },
    ))
}

/// The mirror crates that no manifest records, i.e. crates that have not made a trip yet
async fn unmanifested(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SortParams>,
) -> Result<Markup, StatusCode> {
    let Some(manifests) = load_manifests(&state)? else {
        return Ok(no_manifests_page());
    };

    let crates = manifest::unmanifested(&state.mirror_path, &manifests).map_err(|e| {
        warn!("failed to scan {}: {e:#}", state.mirror_path.display());
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let listed = listing(&state.mirror_path, &crates, params.sort());
    // Every entry comes from a file in the mirror, so none of them can be culled.
    let all: Vec<&Listed<'_>> = listed.iter().collect();

    Ok(page(
        "un-manifested - zerus",
        html! {
            h1 { "Un-manifested" }
            p.count { (all.len()) " crate(s) in the mirror that no manifest records" }
            @if all.is_empty() {
                p.empty { "Every crate in the mirror is in a manifest." }
            } @else {
                (crate_table(&all, "/unmanifested", params.sort()))
            }
        },
    ))
}

#[derive(serde::Deserialize)]
struct ManifestSearchParams {
    q: Option<String>,
}

/// Reverse lookup: which manifest(s) list a crate
async fn manifest_search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ManifestSearchParams>,
) -> Result<Markup, StatusCode> {
    let Some(manifests) = load_manifests(&state)? else {
        return Ok(no_manifests_page());
    };

    let query = params.q.unwrap_or_default();
    let needle = query.to_lowercase();

    let mut hits: Vec<(&str, &str, &str)> = Vec::new();
    if !needle.is_empty() {
        for m in &manifests {
            for c in &m.crates {
                if c.name.to_lowercase().contains(&needle) {
                    hits.push((c.name.as_str(), c.version.as_str(), m.name.as_str()));
                }
            }
        }
        hits.sort_unstable();
    }

    Ok(page_with_search(
        "search - zerus",
        &query,
        Focus::Search,
        html! {
            h1 { "Search" }
            @if query.is_empty() {
                p.empty { "Enter a crate name." }
            } @else if hits.is_empty() {
                p.empty { "No crate matching " code { (query) } " in any manifest." }
            } @else {
                table {
                    thead { tr { th { "crate" } th { "version" } th { "manifest" } } }
                    tbody {
                        @for (name, version, file) in &hits {
                            tr {
                                td { (name) }
                                td { (version) }
                                td { a href={ "/manifests/" (file) } { (file) } }
                            }
                        }
                    }
                }
            }
        },
    ))
}

fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(manifests_index))
        .route("/manifests/{name}", get(manifest_detail))
        .route("/unmanifested", get(unmanifested))
        .route("/search", get(manifest_search))
        .route("/api/v1/crates", get(search))
        .route("/crates/{*path}", get(serve_crate_file))
        .route("/crates.io-index/{*path}", get(serve_index_file))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

pub fn serve(
    mirror_path: PathBuf,
    bind: String,
    manifests_path: Option<PathBuf>,
) -> anyhow::Result<()> {
    let app = router(Arc::new(AppState {
        mirror_path,
        manifests_path,
    }));

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind(&bind).await?;
        info!("serving on http://{bind}");
        axum::serve(listener, app).await?;
        Ok::<_, anyhow::Error>(())
    })?;

    Ok(())
}
