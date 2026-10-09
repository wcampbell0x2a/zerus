use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::SystemTime;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use maud::{html, Markup, PreEscaped, DOCTYPE};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use time::macros::format_description;
use time::OffsetDateTime;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use crate::get_crate_path;
use crate::index::{extract_cargo_toml, IndexEntry};
use crate::manifest::{self, ParsedFile};
use crate::Crate;

struct AppState {
    mirror_path: PathBuf,
    /// Directory of manifest files from previous transfers, if `--manifests` was given
    manifests_path: Option<PathBuf>,
    /// What the manifest pages show. On a network mount, reading the mirror takes one round
    /// trip per crate, so it is read at startup and again only when the manifest list (`/`)
    /// loads. Every other manifest page uses the copy that is here.
    snapshot: RwLock<Arc<Snapshot>>,
}

impl AppState {
    fn new(mirror_path: PathBuf, manifests_path: Option<PathBuf>) -> Self {
        let snapshot = Snapshot::read(&mirror_path, manifests_path.as_deref());
        Self {
            mirror_path,
            manifests_path,
            snapshot: RwLock::new(Arc::new(snapshot)),
        }
    }

    fn snapshot(&self) -> Arc<Snapshot> {
        let snapshot = self.snapshot.read().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(&snapshot)
    }

    /// Read the mirror and the manifests again, and keep the result for the other pages
    fn refresh(&self) -> Arc<Snapshot> {
        let fresh = Arc::new(Snapshot::read(
            &self.mirror_path,
            self.manifests_path.as_deref(),
        ));
        *self.snapshot.write().unwrap_or_else(PoisonError::into_inner) = Arc::clone(&fresh);
        fresh
    }
}

/// Each crate in the mirror, with the write time of its `.crate` file
type WriteTimes = BTreeMap<Crate, SystemTime>;

/// The result of the scan of the mirror `crates/` directory
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MirrorScan {
    Read,
    /// The scan failed, so the mirror shows as empty
    Failed,
}

/// The manifest files, as the last scan found them
enum Manifests {
    /// `--manifests` was not given
    NotGiven,
    /// The directory could not be listed
    Unreadable,
    Files(Vec<ParsedFile>),
}

/// The mirror and the manifests at one point in time
struct Snapshot {
    mirror: WriteTimes,
    scan: MirrorScan,
    manifests: Manifests,
}

impl Snapshot {
    fn read(mirror_path: &Path, manifests_path: Option<&Path>) -> Self {
        let (mirror, scan) = match read_write_times(mirror_path) {
            Ok(mirror) => (mirror, MirrorScan::Read),
            Err(e) => {
                warn!("failed to scan {}: {e:#}", mirror_path.display());
                (WriteTimes::new(), MirrorScan::Failed)
            }
        };

        let manifests = match manifests_path {
            None => Manifests::NotGiven,
            Some(dir) => match manifest::parse_each(dir) {
                Ok(files) => Manifests::Files(files),
                Err(e) => {
                    warn!("failed to read manifests from {}: {e:#}", dir.display());
                    Manifests::Unreadable
                }
            },
        };

        Self {
            mirror,
            scan,
            manifests,
        }
    }

    /// Every manifest as `(file name, crates)`, or `None` if `--manifests` was not given.
    /// Fails if one file did not parse, as a page that uses all of them cannot leave one out.
    fn all_manifests(&self) -> Result<Option<Vec<(&str, &[Crate])>>, StatusCode> {
        let files = match &self.manifests {
            Manifests::NotGiven => return Ok(None),
            Manifests::Unreadable => return Err(StatusCode::INTERNAL_SERVER_ERROR),
            Manifests::Files(files) => files,
        };

        files
            .iter()
            .map(|file| match &file.crates {
                Ok(crates) => Ok((file.name.as_str(), crates.as_slice())),
                Err(e) => {
                    warn!("failed to parse manifest {}: {e:#}", file.name);
                    Err(StatusCode::INTERNAL_SERVER_ERROR)
                }
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    /// Mirror crates that no manifest records
    fn unmanifested<'a>(&'a self, manifests: &[(&str, &'a [Crate])]) -> Vec<&'a Crate> {
        let recorded = manifests.iter().flat_map(|(_, crates)| crates.iter());
        manifest::unmanifested(self.mirror.keys(), recorded)
    }
}

/// Find every `.crate` file in the mirror and read its write time
fn read_write_times(mirror_path: &Path) -> anyhow::Result<WriteTimes> {
    // One stat per crate. On a network mount this is most of the cost, so do them in parallel.
    // A crate culled between the scan and its stat is no longer in the mirror, so it drops out.
    Ok(manifest::generate(mirror_path)?
        .into_par_iter()
        .filter_map(|krate| {
            let written = written_at(mirror_path, &krate.name, &krate.version)?;
            Some((krate, written))
        })
        .collect())
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
}

impl FromStr for Sort {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        [Self::Name, Self::Written]
            .into_iter()
            .find(|sort| sort.param() == value)
            .ok_or(())
    }
}

/// The part of a manifest's crates that a detail page lists
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Show {
    /// Crates still in the mirror, which have yet to make the trip
    Present,
    /// Crates already transferred and removed from the mirror
    Culled,
}

impl Show {
    /// The `show=` value that asks for this part
    fn param(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Culled => "culled",
        }
    }
}

impl FromStr for Show {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        [Self::Present, Self::Culled]
            .into_iter()
            .find(|show| show.param() == value)
            .ok_or(())
    }
}

/// The most rows one page lists. A manifest can hold hundreds of thousands of crates,
/// and a table that large takes a browser a long time to lay out.
const PAGE_SIZE: usize = 1000;

/// A crate in a listing, paired with the write time of its `.crate` file.
/// `written` is `None` for a culled crate, which no longer has a file.
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
fn listing<'a>(
    crates: impl IntoIterator<Item = &'a Crate>,
    mirror: &WriteTimes,
    sort: Sort,
) -> Vec<Listed<'a>> {
    let mut listed: Vec<Listed<'a>> = crates
        .into_iter()
        .map(|krate| Listed {
            krate,
            written: mirror.get(krate).copied(),
        })
        .collect();

    match sort {
        Sort::Name => listed.par_sort_by(|a, b| a.krate.cmp(b.krate)),
        Sort::Written => {
            listed.par_sort_by(|a, b| b.written.cmp(&a.written).then_with(|| a.krate.cmp(b.krate)))
        }
    }

    listed
}

/// Split a listing into the crates still in the mirror and the culled ones, keeping the
/// order `listing` put them in
fn split_culled<'a, 'b>(listed: &'b [Listed<'a>]) -> (Vec<&'b Listed<'a>>, Vec<&'b Listed<'a>>) {
    listed.iter().partition(|l| l.in_mirror())
}

/// One page of a listing
#[derive(Debug, PartialEq, Eq)]
struct Page<'a, T> {
    rows: &'a [T],
    /// 1-based
    number: usize,
    /// Never 0: an empty listing still has one, empty, page
    count: usize,
}

/// Cut page `requested` out of `rows`. A page number out of range gives the nearest page,
/// so a stale link still shows something after the listing shrinks.
fn paginate<T>(rows: &[T], requested: usize) -> Page<'_, T> {
    let count = rows.len().div_ceil(PAGE_SIZE).max(1);
    let number = requested.clamp(1, count);
    let start = (number - 1) * PAGE_SIZE;
    let end = (start + PAGE_SIZE).min(rows.len());

    Page {
        rows: &rows[start..end],
        number,
        count,
    }
}

/// The query of a crate listing page.
/// Each field is `None` for both a missing and an unrecognized value, so a hand-edited URL
/// falls back to the default instead of failing the request.
#[derive(Debug, Default, serde::Deserialize)]
struct ListParams {
    #[serde(default, deserialize_with = "ignore_invalid")]
    sort: Option<Sort>,
    #[serde(default, deserialize_with = "ignore_invalid")]
    show: Option<Show>,
    #[serde(default, deserialize_with = "ignore_invalid")]
    page: Option<usize>,
}

fn ignore_invalid<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: FromStr,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(raw.as_deref().and_then(|value| value.parse().ok()))
}

impl ListParams {
    /// The view these params ask for, of the listing at `base`
    fn view(self, base: &str) -> View<'_> {
        View {
            base,
            sort: self.sort.unwrap_or(Sort::Name),
            show: self.show.unwrap_or(Show::Present),
            page: self.page.unwrap_or(1),
        }
    }
}

/// Everything that picks what a listing page shows. Links on the page change one field and
/// keep the others, so sorting does not lose the culled view and paging does not lose the order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct View<'a> {
    /// The path of the listing, e.g. `/manifests/2026-08-13.txt`
    base: &'a str,
    sort: Sort,
    show: Show,
    /// 1-based
    page: usize,
}

impl View<'_> {
    /// A new order starts back at page 1, as the old page number points at other rows
    fn with_sort(self, sort: Sort) -> Self {
        Self {
            sort,
            page: 1,
            ..self
        }
    }

    fn with_show(self, show: Show) -> Self {
        Self {
            show,
            page: 1,
            ..self
        }
    }

    fn with_page(self, page: usize) -> Self {
        Self { page, ..self }
    }

    /// The URL of this view. Defaults stay out of it, other than `sort`, to keep URLs short.
    fn href(&self) -> String {
        let mut href = format!("{}?sort={}", self.base, self.sort.param());
        if self.show != Show::Present {
            href.push_str("&show=");
            href.push_str(self.show.param());
        }
        if self.page != 1 {
            href.push_str(&format!("&page={}", self.page));
        }
        href
    }
}

/// A crate table with sortable `crate` and `written` column headers
fn crate_table(rows: &[&Listed<'_>], view: View<'_>) -> Markup {
    html! {
        table {
            thead {
                tr {
                    th { (sort_link(view, "crate", Sort::Name)) }
                    th { "version" }
                    th { (sort_link(view, "written", Sort::Written)) }
                }
            }
            tbody {
                @for l in rows {
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

/// One page of `rows` as a table, between links to the pages around it
fn paged_table(rows: &[&Listed<'_>], view: View<'_>) -> Markup {
    let page = paginate(rows, view.page);
    let view = view.with_page(page.number);
    html! {
        (pager(&page, view))
        (crate_table(page.rows, view))
        (pager(&page, view))
    }
}

/// Links to the previous and next page. Empty markup when everything fits on one page.
fn pager<T>(page: &Page<'_, T>, view: View<'_>) -> Markup {
    html! {
        @if page.count > 1 {
            nav.pages {
                @if page.number > 1 {
                    a href=(view.with_page(1).href()) { "first" }
                    a href=(view.with_page(page.number - 1).href()) { "prev" }
                }
                span.count { "page " (page.number) " of " (page.count) }
                @if page.number < page.count {
                    a href=(view.with_page(page.number + 1).href()) { "next" }
                    a href=(view.with_page(page.count).href()) { "last" }
                }
            }
        }
    }
}

/// Column header that re-requests the page sorted by `column`
fn sort_link(view: View<'_>, label: &str, column: Sort) -> Markup {
    html! {
        a.sort.active[column == view.sort] href=(view.with_sort(column).href()) { (label) }
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
nav.pages { margin: .75rem 0; }
nav.pages a, nav.pages span { margin-right: .75rem; }
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
    blocking(move || manifests_index_page(&state)).await
}

fn manifests_index_page(state: &AppState) -> Result<Markup, StatusCode> {
    // The one page that reads the disk, so the user can pick up new transfers and culls
    let snapshot = state.refresh();
    let Some(manifests) = snapshot.all_manifests()? else {
        return Ok(no_manifests_page());
    };

    // A crate is only in the mirror because someone put it there, so this page always
    // offers the un-manifested view, even when no manifest file exists yet.
    let unmanifested_count = snapshot.unmanifested(&manifests).len();

    Ok(page(
        "crates - zerus",
        html! {
            h1 { "Crates" }
            table {
                thead { tr { th { "manifest" } th { "crates" } } }
                tbody {
                    @for (name, crates) in &manifests {
                        tr {
                            td { a href={ "/manifests/" (name) } { (name) } }
                            td.count { (crates.len()) }
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

/// Detail: the crates listed in one manifest, split into present and culled
async fn manifest_detail(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
    Query(params): Query<ListParams>,
) -> Result<Markup, StatusCode> {
    blocking(move || manifest_detail_page(&state, &name, params)).await
}

fn manifest_detail_page(
    state: &AppState,
    name: &str,
    params: ListParams,
) -> Result<Markup, StatusCode> {
    let snapshot = state.snapshot();
    let files = match &snapshot.manifests {
        Manifests::NotGiven => return Ok(no_manifests_page()),
        Manifests::Unreadable => return Err(StatusCode::INTERNAL_SERVER_ERROR),
        Manifests::Files(files) => files,
    };

    // Match against the listing, so a name such as `../secret` cannot reach another file.
    // A bad file elsewhere does not stop this page, only the bad file's own page.
    let found = files
        .iter()
        .find(|file| file.name == name)
        .ok_or(StatusCode::NOT_FOUND)?;
    let crates = found.crates.as_ref().map_err(|e| {
        warn!("failed to parse manifest {name}: {e:#}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let base = format!("/manifests/{name}");
    let view = params.view(&base);
    let listed = listing(crates, &snapshot.mirror, view.sort);
    let (present, culled) = split_culled(&listed);

    Ok(page(
        &format!("{name} - zerus"),
        html! {
            h1 { (name) }
            p.count {
                (listed.len()) " crate(s), " (present.len()) " still in mirror, "
                (culled.len()) " culled"
            }
            @match view.show {
                Show::Present => {
                    @if !culled.is_empty() {
                        p { a href=(view.with_show(Show::Culled).href()) {
                            "show culled (" (culled.len()) ")"
                        } }
                    }
                    @if present.is_empty() {
                        p.empty { "Every crate in this manifest was culled." }
                    } @else {
                        (paged_table(&present, view))
                    }
                }
                Show::Culled => {
                    p { a href=(view.with_show(Show::Present).href()) {
                        "show still in mirror (" (present.len()) ")"
                    } }
                    @if culled.is_empty() {
                        p.empty { "No crate in this manifest was culled." }
                    } @else {
                        (paged_table(&culled, view))
                    }
                }
            }
        },
    ))
}

/// The mirror crates that no manifest records, i.e. crates that have not made a trip yet
async fn unmanifested(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
) -> Result<Markup, StatusCode> {
    blocking(move || unmanifested_page(&state, params)).await
}

fn unmanifested_page(state: &AppState, params: ListParams) -> Result<Markup, StatusCode> {
    let snapshot = state.snapshot();
    let Some(manifests) = snapshot.all_manifests()? else {
        return Ok(no_manifests_page());
    };
    if snapshot.scan == MirrorScan::Failed {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    // Every entry comes from a file in the mirror, so none of them can be culled.
    let view = ListParams {
        show: None,
        ..params
    }
    .view("/unmanifested");
    let listed = listing(
        snapshot.unmanifested(&manifests),
        &snapshot.mirror,
        view.sort,
    );
    let all: Vec<&Listed<'_>> = listed.iter().collect();

    Ok(page(
        "un-manifested - zerus",
        html! {
            h1 { "Un-manifested" }
            p.count { (all.len()) " crate(s) in the mirror that no manifest records" }
            @if all.is_empty() {
                p.empty { "Every crate in the mirror is in a manifest." }
            } @else {
                (paged_table(&all, view))
            }
        },
    ))
}

/// Run blocking page work on the blocking thread pool. The manifest pages read and stat
/// many files; on the async runtime, a slow disk would stall every other request too.
async fn blocking(
    render: impl FnOnce() -> Result<Markup, StatusCode> + Send + 'static,
) -> Result<Markup, StatusCode> {
    tokio::task::spawn_blocking(render)
        .await
        .unwrap_or_else(|e| {
            warn!("page render task failed: {e}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        })
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
    blocking(move || manifest_search_page(&state, params)).await
}

fn manifest_search_page(
    state: &AppState,
    params: ManifestSearchParams,
) -> Result<Markup, StatusCode> {
    let snapshot = state.snapshot();
    let Some(manifests) = snapshot.all_manifests()? else {
        return Ok(no_manifests_page());
    };

    let query = params.q.unwrap_or_default();
    let needle = query.to_lowercase();

    let mut hits: Vec<(&str, &str, &str)> = Vec::new();
    if !needle.is_empty() {
        for (file, crates) in &manifests {
            for c in crates.iter() {
                if c.name.to_lowercase().contains(&needle) {
                    hits.push((c.name.as_str(), c.version.as_str(), *file));
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
    info!("reading the mirror and the manifests");
    let app = router(Arc::new(AppState::new(mirror_path, manifests_path)));

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind(&bind).await?;
        info!("serving on http://{bind}");
        axum::serve(listener, app).await?;
        Ok::<_, anyhow::Error>(())
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use axum::extract::Query;
    use axum::http::Uri;

    use super::*;

    fn params(query: &str) -> ListParams {
        let uri: Uri = format!("/x?{query}").parse().unwrap();
        Query::<ListParams>::try_from_uri(&uri).unwrap().0
    }

    #[test]
    fn list_params_parse_known_values() {
        let p = params("sort=written&show=culled&page=3");
        assert_eq!(p.sort, Some(Sort::Written));
        assert_eq!(p.show, Some(Show::Culled));
        assert_eq!(p.page, Some(3));
    }

    #[test]
    fn list_params_ignore_bad_and_missing_values() {
        for query in ["", "sort=zzz&show=all&page=abc", "page=-1", "page=", "sort=Name"] {
            let p = params(query);
            assert_eq!(p.sort, None, "{query}");
            assert_eq!(p.show, None, "{query}");
            assert_eq!(p.page, None, "{query}");
        }
    }

    #[test]
    fn view_defaults() {
        let view = ListParams::default().view("/b");
        assert_eq!(view.sort, Sort::Name);
        assert_eq!(view.show, Show::Present);
        assert_eq!(view.page, 1);
        assert_eq!(view.href(), "/b?sort=name");
    }

    #[test]
    fn view_href_keeps_every_non_default_field() {
        let view = params("sort=written&show=culled&page=4").view("/manifests/a.txt");
        assert_eq!(
            view.href(),
            "/manifests/a.txt?sort=written&show=culled&page=4"
        );
    }

    #[test]
    fn view_href_round_trips_through_list_params() {
        for sort in [Sort::Name, Sort::Written] {
            for show in [Show::Present, Show::Culled] {
                for page in [1, 2, 99] {
                    let view = View {
                        base: "/b",
                        sort,
                        show,
                        page,
                    };
                    let query = view.href().split_once('?').unwrap().1.to_string();
                    assert_eq!(params(&query).view("/b"), view);
                }
            }
        }
    }

    #[test]
    fn new_sort_or_show_starts_at_page_one_and_keeps_the_rest() {
        let view = params("sort=written&show=culled&page=4").view("/b");

        let sorted = view.with_sort(Sort::Name);
        assert_eq!(
            (sorted.sort, sorted.show, sorted.page),
            (Sort::Name, Show::Culled, 1)
        );

        let shown = view.with_show(Show::Present);
        assert_eq!(
            (shown.sort, shown.show, shown.page),
            (Sort::Written, Show::Present, 1)
        );
    }

    #[test]
    fn paginate_empty_is_one_empty_page() {
        let page = paginate::<u8>(&[], 1);
        assert_eq!((page.rows.len(), page.number, page.count), (0, 1, 1));
    }

    #[test]
    fn paginate_clamps_out_of_range_pages() {
        let rows: Vec<usize> = (0..PAGE_SIZE * 2 + 1).collect();

        let low = paginate(&rows, 0);
        assert_eq!((low.number, low.rows[0]), (1, 0));

        let high = paginate(&rows, usize::MAX);
        assert_eq!((high.number, high.count), (3, 3));
        assert_eq!(high.rows, [PAGE_SIZE * 2]);
    }

    /// Model check: for any length, the pages split the rows in order with no gap or overlap
    #[test]
    fn paginate_pages_cover_the_rows_exactly_once() {
        let lengths = (0..=3)
            .flat_map(|n| [n * PAGE_SIZE, n * PAGE_SIZE + 1, (n + 1) * PAGE_SIZE - 1])
            .chain([7, 999, 1001, 2500]);
        for len in lengths {
            let rows: Vec<usize> = (0..len).collect();
            let count = paginate(&rows, 1).count;
            assert_eq!(count, len.div_ceil(PAGE_SIZE).max(1), "len {len}");

            let joined: Vec<usize> = (1..=count)
                .flat_map(|n| {
                    let page = paginate(&rows, n);
                    assert_eq!(page.number, n);
                    assert!(page.rows.len() <= PAGE_SIZE);
                    page.rows.to_vec()
                })
                .collect();
            assert_eq!(joined, rows, "len {len}");
        }
    }

    fn add_crate(mirror: &Path, name: &str, version: &str) {
        let dir = get_crate_path(mirror, name, version).unwrap();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{name}-{version}.crate")), b"x").unwrap();
    }

    /// A mirror holding `serde` and `tokio`, and a manifest that also lists the culled `axum`
    fn fixture() -> (tempfile::TempDir, AppState) {
        let tmp = tempfile::tempdir().unwrap();
        let mirror = tmp.path().join("mirror");
        let manifests = tmp.path().join("manifests");
        fs::create_dir(&manifests).unwrap();
        add_crate(&mirror, "serde", "1.0.210");
        add_crate(&mirror, "tokio", "1.40.0");
        fs::write(
            manifests.join("a.txt"),
            "tokio@1.40.0\nserde@1.0.210\naxum@0.8.1\n",
        )
        .unwrap();
        // A broken manifest elsewhere must not break the page of `a.txt`
        fs::write(manifests.join("broken.txt"), "nope\n").unwrap();

        (tmp, AppState::new(mirror, Some(manifests)))
    }

    /// Write a manifest file and read it into the cache
    fn add_manifest(state: &AppState, name: &str, contents: &str) {
        let dir = state.manifests_path.as_deref().unwrap();
        fs::write(dir.join(name), contents).unwrap();
        state.refresh();
    }

    fn detail(state: &AppState, name: &str, query: &str) -> Result<String, StatusCode> {
        manifest_detail_page(state, name, params(query)).map(Markup::into_string)
    }

    #[test]
    fn detail_lists_present_crates_and_links_to_culled() {
        let (_tmp, state) = fixture();

        let html = detail(&state, "a.txt", "").unwrap();

        assert!(
            html.contains("3 crate(s), 2 still in mirror, 1 culled"),
            "{html}"
        );
        assert!(html.contains("serde") && html.contains("tokio"));
        assert!(!html.contains("axum"));
        assert!(html.contains(r#"href="/manifests/a.txt?sort=name&amp;show=culled""#));
        assert!(html.contains("show culled (1)"));
        // Name order, not file order
        assert!(html.find("serde") < html.find("tokio"));
        // Everything fits on one page
        assert!(!html.contains("nav class=\"pages\""));
    }

    #[test]
    fn detail_show_culled_lists_only_culled_crates() {
        let (_tmp, state) = fixture();

        let html = detail(&state, "a.txt", "show=culled").unwrap();

        assert!(html.contains("axum"));
        assert!(!html.contains("<td>serde</td>"));
        assert!(html.contains("show still in mirror (2)"));
    }

    #[test]
    fn detail_with_nothing_culled_has_no_culled_link() {
        let (_tmp, state) = fixture();
        add_manifest(&state, "b.txt", "serde@1.0.210\n");

        let html = detail(&state, "b.txt", "").unwrap();
        assert!(!html.contains("show culled"));

        let html = detail(&state, "b.txt", "show=culled").unwrap();
        assert!(html.contains("No crate in this manifest was culled."));
    }

    #[test]
    fn detail_with_everything_culled_says_so() {
        let (_tmp, state) = fixture();
        add_manifest(&state, "b.txt", "axum@0.8.1\n");

        let html = detail(&state, "b.txt", "").unwrap();

        assert!(html.contains("Every crate in this manifest was culled."));
        assert!(html.contains("show culled (1)"));
    }

    #[test]
    fn detail_errors() {
        let (_tmp, state) = fixture();
        assert_eq!(detail(&state, "nope.txt", ""), Err(StatusCode::NOT_FOUND));
        assert_eq!(detail(&state, "../mirror", ""), Err(StatusCode::NOT_FOUND));
        assert_eq!(
            detail(&state, "broken.txt", ""),
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        );

        let no_dir = AppState::new(state.mirror_path.clone(), None);
        let html = detail(&no_dir, "a.txt", "").unwrap();
        assert!(html.contains("No manifests directory"));

        let gone_dir = AppState::new(
            state.mirror_path.clone(),
            Some(state.mirror_path.join("nope")),
        );
        assert_eq!(
            detail(&gone_dir, "a.txt", ""),
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        );
    }

    #[test]
    fn manifest_pages_use_the_cache_until_the_list_loads() {
        let (_tmp, state) = fixture();
        let dir = state.manifests_path.as_deref().unwrap().to_path_buf();
        fs::remove_file(dir.join("broken.txt")).unwrap();
        fs::write(dir.join("b.txt"), "tokio@1.40.0\n").unwrap();
        // Cull `tokio` from the mirror
        let tokio = get_crate_path(&state.mirror_path, "tokio", "1.40.0").unwrap();
        fs::remove_dir_all(&tokio).unwrap();

        // The cache is from before the changes
        assert_eq!(detail(&state, "b.txt", ""), Err(StatusCode::NOT_FOUND));
        let html = detail(&state, "a.txt", "").unwrap();
        assert!(html.contains("2 still in mirror, 1 culled"), "{html}");

        let list = manifests_index_page(&state).unwrap().into_string();
        assert!(list.contains("b.txt"), "{list}");

        // The list read the disk again, so the other pages now show the changes
        let html = detail(&state, "b.txt", "").unwrap();
        assert!(html.contains("1 crate(s), 0 still in mirror, 1 culled"), "{html}");
        let html = detail(&state, "a.txt", "").unwrap();
        assert!(html.contains("1 still in mirror, 2 culled"), "{html}");
    }

    #[test]
    fn manifest_list_with_a_bad_file_fails_but_still_refreshes() {
        let (_tmp, state) = fixture();
        let dir = state.manifests_path.as_deref().unwrap().to_path_buf();
        fs::write(dir.join("b.txt"), "serde@1.0.210\n").unwrap();

        // `broken.txt` fails the list, as the list must show every file
        assert_eq!(
            manifests_index_page(&state).map(Markup::into_string),
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        );
        assert!(detail(&state, "b.txt", "").is_ok());
    }

    #[test]
    fn manifest_list_counts_crates_and_unmanifested() {
        let (_tmp, state) = fixture();
        let dir = state.manifests_path.as_deref().unwrap().to_path_buf();
        fs::remove_file(dir.join("broken.txt")).unwrap();
        fs::write(dir.join("a.txt"), "serde@1.0.210\naxum@0.8.1\n").unwrap();

        let list = manifests_index_page(&state).unwrap().into_string();

        assert!(list.contains(r#"<a href="/manifests/a.txt">a.txt</a>"#), "{list}");
        assert!(list.contains(r#"<td class="count">2</td>"#), "{list}");
        // `tokio` is in the mirror but in no manifest
        assert!(list.contains(r#"<td class="count">1</td>"#), "{list}");
    }

    #[test]
    fn search_finds_crates_in_the_cached_manifests() {
        let (_tmp, state) = fixture();
        let dir = state.manifests_path.as_deref().unwrap().to_path_buf();
        fs::remove_file(dir.join("broken.txt")).unwrap();
        state.refresh();

        let html = manifest_search_page(
            &state,
            ManifestSearchParams {
                q: Some("SER".to_string()),
            },
        )
        .unwrap()
        .into_string();

        assert!(html.contains("<td>serde</td>"), "{html}");
        assert!(!html.contains("<td>tokio</td>"), "{html}");
    }

    #[test]
    fn unmanifested_fails_when_the_mirror_scan_failed() {
        let (tmp, state) = fixture();
        let dir = state.manifests_path.as_deref().unwrap().to_path_buf();
        fs::remove_file(dir.join("broken.txt")).unwrap();
        let no_mirror = AppState::new(tmp.path().join("nope"), Some(dir));

        assert_eq!(
            unmanifested_page(&no_mirror, params("")).map(Markup::into_string),
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        );
    }

    #[test]
    fn detail_pages_a_large_manifest() {
        let (_tmp, state) = fixture();
        let lines: String = (0..PAGE_SIZE + 5)
            .map(|n| format!("gone{n:05}@1.0.0\n"))
            .collect();
        add_manifest(&state, "big.txt", &lines);

        let first = detail(&state, "big.txt", "show=culled").unwrap();
        assert!(first.contains("page 1 of 2"));
        assert!(first.contains("<td>gone00000</td>"));
        assert!(!first.contains(&format!("<td>gone{PAGE_SIZE:05}</td>")));
        assert!(first.contains(r#"href="/manifests/big.txt?sort=name&amp;show=culled&amp;page=2""#));

        let last = detail(&state, "big.txt", "show=culled&page=2").unwrap();
        assert!(last.contains("page 2 of 2"));
        assert!(last.contains(&format!("<td>gone{PAGE_SIZE:05}</td>")));
        assert!(!last.contains("<td>gone00000</td>"));
    }

    #[test]
    fn unmanifested_ignores_show_culled() {
        let (_tmp, state) = fixture();
        let dir = state.manifests_path.as_deref().unwrap();
        fs::remove_file(dir.join("broken.txt")).unwrap();
        add_manifest(&state, "a.txt", "serde@1.0.210\n");

        let html = unmanifested_page(&state, params("show=culled"))
            .unwrap()
            .into_string();

        assert!(html.contains("1 crate(s) in the mirror"));
        assert!(html.contains("<td>tokio</td>"));
        assert!(html.contains(r#"href="/unmanifested?sort=written""#));
    }
}
