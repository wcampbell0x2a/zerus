use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
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
    /// The manifest pages, if `--manifests` was given
    browse: Option<Arc<Browse>>,
}

impl AppState {
    fn new(
        mirror_path: PathBuf,
        manifests_path: Option<PathBuf>,
        rescan: Rescan,
    ) -> anyhow::Result<Self> {
        let browse = manifests_path
            .map(|manifests_path| Browse::new(mirror_path.clone(), manifests_path, rescan))
            .transpose()?
            .map(Arc::new);
        Ok(Self {
            mirror_path,
            browse,
        })
    }
}

/// When a page load starts a scan on its own
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rescan {
    /// Only the Refresh button starts a scan
    Never,
    /// A page load starts a background scan when its data is older than this. The scan runs
    /// only when someone looks, so an idle server does no work on the disk.
    After(Duration),
}

impl FromStr for Rescan {
    type Err = String;

    /// Parse `0`, or a whole number with the unit `s`, `m`, or `h`, e.g. `10m`
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let bad = || format!("expected 0, or a duration such as 30s, 10m, or 1h, found {value:?}");
        if value == "0" {
            return Ok(Self::Never);
        }
        let unit_at = value
            .find(|c: char| !c.is_ascii_digit())
            .filter(|&at| at > 0)
            .ok_or_else(bad)?;
        let (number, unit) = value.split_at(unit_at);
        let number: u64 = number.parse().map_err(|_| bad())?;
        let unit_secs = match unit {
            "s" => 1,
            "m" => 60,
            "h" => 3600,
            _ => return Err(bad()),
        };
        match number.checked_mul(unit_secs).ok_or_else(bad)? {
            0 => Ok(Self::Never),
            secs => Ok(Self::After(Duration::from_secs(secs))),
        }
    }
}

/// Threads in the scan pool. A scan waits on the disk and not on the CPU: on a network mount,
/// each stat is a round trip. More threads keep more of these round trips in flight.
const SCAN_THREADS: usize = 64;

/// The data that the manifest pages show, and the scans that keep it current
struct Browse {
    mirror_path: PathBuf,
    /// Directory of manifest files from previous transfers
    manifests_path: PathBuf,
    /// The result of the last scan, or `None` until the first scan is done. On a network mount,
    /// reading the mirror takes one round trip per crate, so a scan runs only at startup, when
    /// the user clicks Refresh, and when a page load finds the data stale. Every manifest page
    /// uses the copy that is here.
    snapshot: RwLock<Option<Arc<Snapshot>>>,
    /// When the running scan started, or `None` if no scan runs. Only one scan runs at a time,
    /// so many refreshes at the same time do not each scan the whole mirror.
    running: Mutex<Option<SystemTime>>,
    rescan: Rescan,
    /// The scans run on this pool and not on the global one. Each page sorts its rows on the
    /// global pool, and a scan there would make every page wait for the scan to finish.
    scan_pool: rayon::ThreadPool,
}

/// What a request to scan did
#[derive(Debug)]
enum ScanStart {
    /// A new scan runs on this thread
    // Only the tests wait for a scan to end
    Started(#[cfg_attr(not(test), allow(dead_code))] std::thread::JoinHandle<()>),
    /// A scan was already running, so no new one started
    AlreadyRunning,
}

/// The scans as one request sees them
struct Scans {
    /// The result of the last scan, or `None` until the first scan is done
    shown: Option<Arc<Snapshot>>,
    /// When the running scan started, if a scan runs
    running: Option<SystemTime>,
}

impl Browse {
    fn new(mirror_path: PathBuf, manifests_path: PathBuf, rescan: Rescan) -> anyhow::Result<Self> {
        let scan_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(SCAN_THREADS)
            .thread_name(|n| format!("scan-{n}"))
            .build()
            .context("failed to start the scan thread pool")?;

        Ok(Self {
            mirror_path,
            manifests_path,
            snapshot: RwLock::new(None),
            running: Mutex::new(None),
            rescan,
            scan_pool,
        })
    }

    fn running(&self) -> std::sync::MutexGuard<'_, Option<SystemTime>> {
        self.running.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn scans(&self) -> Scans {
        // Read `running` first. A scan stores its snapshot before it clears `running`, so if
        // this sees no running scan, the snapshot read next is at least as new as that scan.
        let running = *self.running();
        let shown = self
            .snapshot
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(Arc::clone);
        Scans { shown, running }
    }

    /// Is the data in `scans` so old at `now` that a page load must start a scan?
    /// No data at all, e.g. after a failed start, is old too.
    fn is_stale(&self, scans: &Scans, now: SystemTime) -> bool {
        let Rescan::After(limit) = self.rescan else {
            return false;
        };
        let fresh = scans
            .shown
            .as_ref()
            .is_some_and(|shown| now.duration_since(shown.started).unwrap_or_default() < limit);
        scans.running.is_none() && !fresh
    }

    /// Start a scan of the mirror and the manifests on a new thread, unless one already runs.
    /// The request does not wait for the scan: on a network mount, a scan can take minutes.
    fn start_scan(self: &Arc<Self>) -> std::io::Result<ScanStart> {
        let mut running = self.running();
        if running.is_some() {
            return Ok(ScanStart::AlreadyRunning);
        }
        *running = Some(SystemTime::now());
        drop(running);

        // The thread owns the guard. If the spawn fails, the closure drops, and so does the guard.
        let done = ScanDone(Arc::clone(self));
        let handle = std::thread::Builder::new()
            .name("scan".to_string())
            .spawn(move || {
                let browse = &done.0;
                let fresh = browse
                    .scan_pool
                    .install(|| Snapshot::read(&browse.mirror_path, &browse.manifests_path));
                *browse
                    .snapshot
                    .write()
                    .unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(fresh));
            })?;
        Ok(ScanStart::Started(handle))
    }
}

/// Marks the scan as done when the scan thread ends, also when the scan panics. Without it,
/// a panic would leave `running` set, and no scan could start again.
struct ScanDone(Arc<Browse>);

impl Drop for ScanDone {
    fn drop(&mut self) {
        *self.0.running() = None;
    }
}

/// Each crate in the mirror, with the write time of its `.crate` file
type WriteTimes = BTreeMap<Crate, SystemTime>;

/// The crates in the mirror, as the last scan found them
enum Mirror {
    Read(WriteTimes),
    /// The scan failed, so which crates are in the mirror is not known
    Failed,
}

/// The manifest files, as the last scan found them
enum Manifests {
    /// The directory could not be listed
    Unreadable,
    Files(Vec<ParsedFile>),
}

/// Why the un-manifested crates are not known
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnmanifestedUnknown {
    MirrorScanFailed,
    ManifestsUnreadable,
}

impl UnmanifestedUnknown {
    fn reason(self) -> &'static str {
        match self {
            Self::MirrorScanFailed => "the last scan could not read the mirror",
            Self::ManifestsUnreadable => "the last scan could not read the manifests directory",
        }
    }
}

/// The mirror and the manifests at one point in time
struct Snapshot {
    mirror: Mirror,
    manifests: Manifests,
    /// Mirror crates that no manifest records, in name order. The scan finds them once, so
    /// that each load of the manifest list does not compare every manifest with the mirror.
    unmanifested: Result<Vec<Crate>, UnmanifestedUnknown>,
    /// For each manifest that parsed, how many of its crates are still in the mirror. Empty
    /// when the mirror scan failed. The scan counts them once, for the same reason.
    in_mirror: HashMap<String, usize>,
    /// When the scan started. Changes to the disk after this time can be missing.
    started: SystemTime,
    took: Duration,
}

impl Snapshot {
    fn read(mirror_path: &Path, manifests_path: &Path) -> Self {
        let started = SystemTime::now();
        let clock = Instant::now();
        info!("scanning the mirror at {}", mirror_path.display());

        let mirror = match read_write_times(mirror_path) {
            Ok(mirror) => {
                info!(
                    "found {} crate(s) in the mirror in {:.1?}",
                    mirror.len(),
                    clock.elapsed()
                );
                Mirror::Read(mirror)
            }
            Err(e) => {
                warn!("failed to scan {}: {e:#}", mirror_path.display());
                Mirror::Failed
            }
        };
        let manifests = read_manifests(manifests_path);
        let unmanifested = find_unmanifested(&mirror, &manifests);
        let in_mirror = count_in_mirror(&mirror, &manifests);

        let took = clock.elapsed();
        info!("scan done in {took:.1?}");

        Self {
            mirror,
            manifests,
            unmanifested,
            in_mirror,
            started,
            took,
        }
    }
}

impl Snapshot {
    /// How many manifest files did not parse
    fn unparsed(&self) -> usize {
        match &self.manifests {
            Manifests::Unreadable => 0,
            Manifests::Files(files) => files.iter().filter(|file| file.crates.is_err()).count(),
        }
    }
}

/// Tells that the un-manifested list can hold crates that the `unparsed` bad files record
fn unparsed_warning(unparsed: usize) -> Markup {
    html! {
        @if unparsed > 0 {
            p.error {
                (unparsed) " manifest(s) did not parse, so the un-manifested crates can include "
                "crates that they record. Correct the file(s), then click Refresh."
            }
        }
    }
}

/// Mirror crates that no manifest that parsed records. This is known only when the scan read
/// the mirror and listed the manifests.
///
/// A manifest that did not parse does not hide the list: the list is the next transfer, so it
/// is better to show too many crates than none. The pages tell the user about the bad file.
fn find_unmanifested(
    mirror: &Mirror,
    manifests: &Manifests,
) -> Result<Vec<Crate>, UnmanifestedUnknown> {
    let Mirror::Read(mirror) = mirror else {
        return Err(UnmanifestedUnknown::MirrorScanFailed);
    };
    let Manifests::Files(files) = manifests else {
        return Err(UnmanifestedUnknown::ManifestsUnreadable);
    };
    let recorded = files
        .iter()
        .filter_map(|file| file.crates.as_ref().ok())
        .flatten();

    Ok(manifest::unmanifested(mirror.keys(), recorded)
        .into_iter()
        .cloned()
        .collect())
}

/// For each manifest that parsed, how many of its crates are still in the mirror
fn count_in_mirror(mirror: &Mirror, manifests: &Manifests) -> HashMap<String, usize> {
    let (Mirror::Read(mirror), Manifests::Files(files)) = (mirror, manifests) else {
        return HashMap::new();
    };
    files
        .par_iter()
        .filter_map(|file| {
            let crates = file.crates.as_ref().ok()?;
            let present = crates.iter().filter(|c| mirror.contains_key(*c)).count();
            Some((file.name.clone(), present))
        })
        .collect()
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

/// Parse each manifest file in `dir`, and log what each one holds
fn read_manifests(dir: &Path) -> Manifests {
    let clock = Instant::now();
    info!("parsing the manifests in {}", dir.display());

    let files = match manifest::parse_each(dir) {
        Ok(files) => files,
        Err(e) => {
            warn!("failed to read manifests from {}: {e:#}", dir.display());
            return Manifests::Unreadable;
        }
    };

    let mut failed = 0;
    for file in &files {
        match &file.crates {
            Ok(crates) => info!("parsed manifest {}: {} crate(s)", file.name, crates.len()),
            Err(e) => {
                failed += 1;
                warn!("failed to parse manifest {}: {e:#}", file.name);
            }
        }
    }
    info!(
        "parsed {} manifest file(s), {failed} failed, in {:.1?}",
        files.len(),
        clock.elapsed()
    );

    Manifests::Files(files)
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

/// Render a time as `YYYY-MM-DD HH:MM:SS UTC`
fn format_time_with_seconds(time: SystemTime) -> String {
    OffsetDateTime::from(time)
        .format(format_description!(
            "[year]-[month]-[day] [hour]:[minute]:[second] UTC"
        ))
        .unwrap_or_default()
}

/// How long before `now` the time `then` was, in the largest unit that fits
fn ago(then: SystemTime, now: SystemTime) -> String {
    let secs = now.duration_since(then).unwrap_or_default().as_secs();
    match secs {
        0..=59 => format!("{secs} s ago"),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

/// `n` with its digits in groups of three, e.g. `1,234,567`, so that large counts are easy
/// to read
fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Does the page reload itself?
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reload {
    Never,
    /// A scan runs, so reload once, when the scan is done
    WhenScanDone,
}

/// Without JavaScript, the page cannot ask when the scan is done, so it reloads this often.
/// The reload goes back to the top of the page, so it is not short.
const NOSCRIPT_RELOAD_SECS: u32 = 15;

// Ask the server each 2 s if the scan still runs, and reload once when it is done. A reload
// on a timer would jump back to the top of a long page while the user reads it.
const SCAN_WAIT_JS: &str = "\
(function poll() {
  setTimeout(function () {
    fetch('/scan-status')
      .then(function (r) { return r.json(); })
      .then(function (s) { if (s.running) { poll(); } else { location.reload(); } })
      .catch(poll);
  }, 2000);
})();
";

/// The part of the page header that tells how old the data is
#[derive(Clone)]
struct StatusBar {
    markup: Markup,
    reload: Reload,
}

/// Tells how old the data on a page is. Gives a Refresh button that scans again and then
/// comes back to `back`, the path of this page, or tells that a scan runs.
fn status_bar(scans: &Scans, back: &str, now: SystemTime) -> StatusBar {
    let markup = html! {
        div.status {
            @if let Some(shown) = &scans.shown {
                span title=(format!(
                    "The scan started {} and took {:.1} s. Changes to the disk after the start can be missing.",
                    format_time_with_seconds(shown.started),
                    shown.took.as_secs_f64()
                )) {
                    "data from " (ago(shown.started, now))
                }
                " · "
            }
            @match scans.running {
                Some(started) => span {
                    "scan running since " (format_time_with_seconds(started))
                    "… the page reloads when it is done"
                },
                None => (refresh_button(back)),
            }
        }
    };
    let reload = match scans.running {
        Some(_) => Reload::WhenScanDone,
        None => Reload::Never,
    };
    StatusBar { markup, reload }
}

/// A POST and not a link, so that a reload, a bookmark, or a link prefetch does not start a
/// scan of the whole mirror
fn refresh_button(back: &str) -> Markup {
    html! {
        form.refresh action="/refresh" method="post" {
            input type="hidden" name="back" value=(back);
            button.link type="submit" { "Refresh" }
        }
    }
}

/// `back` if it is a path on this server, else `/`. This stops the refresh form from sending
/// the user to another site, and it keeps the redirect a valid header value.
fn local_path(back: Option<&str>) -> &str {
    match back {
        Some(path)
            if path.starts_with('/')
                && !path.starts_with("//")
                && !path.starts_with("/\\")
                && path.bytes().all(|b| b.is_ascii_graphic()) =>
        {
            path
        }
        _ => "/",
    }
}

/// Percent-encode `value` for use in a URL query
fn encode_query(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
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
                    th { (sort_link(view, "written (UTC)", Sort::Written)) }
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
                span.count { "page " (grouped(page.number)) " of " (grouped(page.count)) }
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

fn page(title: &str, status: Option<StatusBar>, body: Markup) -> Markup {
    page_with_search(title, "", Focus::Page, status, body)
}

/// The page shell. The header holds the links, the search box, and the scan status, so they
/// are on every page.
fn page_with_search(
    title: &str,
    query: &str,
    focus: Focus,
    status: Option<StatusBar>,
    body: Markup,
) -> Markup {
    let reload = status
        .as_ref()
        .map_or(Reload::Never, |status| status.reload);
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                @if reload == Reload::WhenScanDone {
                    noscript { meta http-equiv="refresh" content=(NOSCRIPT_RELOAD_SECS); }
                }
                title { (title) }
                style { (PAGE_CSS) }
            }
            body {
                header {
                    nav.site {
                        a.home href="/" { "zerus" }
                        span.version { "v" (env!("CARGO_PKG_VERSION")) }
                        a href="/" { "manifests" }
                        a href="/unmanifested" { "un-manifested" }
                    }
                    (search_form(query, focus))
                    @if let Some(status) = status {
                        (status.markup)
                    }
                }
                main { (body) }
                script { (PreEscaped(SEARCH_SHORTCUT_JS)) }
                @if reload == Reload::WhenScanDone {
                    script { (PreEscaped(SCAN_WAIT_JS)) }
                }
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
header { border-bottom: 1px solid currentColor; margin-bottom: 1.5rem; padding-bottom: .5rem;
         display: flex; flex-wrap: wrap; align-items: baseline; gap: .5rem 1rem; }
nav.site a { margin-right: 1rem; color: inherit; }
nav.site a.home { font-weight: bold; text-decoration: none; margin-right: .5rem; }
header .version { margin-right: 1.5rem; font-size: .85em;
                  color: color-mix(in srgb, currentColor 65%, transparent); }
.status { flex-basis: 100%; text-align: right; font-size: .85em;
          color: color-mix(in srgb, currentColor 65%, transparent); }
button.link { font: inherit; color: inherit; background: none; border: 0; padding: 0;
              text-decoration: underline; cursor: pointer; }
table { border-collapse: collapse; width: 100%; }
td, th { text-align: left; padding: .25rem .75rem .25rem 0; }
th { border-bottom: 1px solid currentColor; }
tr + tr td { border-top: 1px solid color-mix(in srgb, currentColor 15%, transparent); }
.culled { opacity: .55; }
.count { color: color-mix(in srgb, currentColor 65%, transparent); }
td.num { font-variant-numeric: tabular-nums; }
input[type=search] { font: inherit; padding: .3rem; min-width: 16rem; }
p.empty { color: color-mix(in srgb, currentColor 65%, transparent); }
p.crumbs { margin-bottom: 0; }
nav.pages { margin: .75rem 0; }
nav.pages a, nav.pages span { margin-right: .75rem; }
a.sort { color: inherit; text-decoration: none; }
a.sort:hover { text-decoration: underline; }
a.sort.active::after { content: ' \\2193'; }
form.search, form.refresh { display: inline; }
form.search { margin-left: auto; }
.error { color: #c62828; }
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

/// A page in place of the one that the request asked for, with the status that tells why
#[derive(Debug)]
struct ErrorPage {
    status: StatusCode,
    body: Markup,
}

impl ErrorPage {
    fn new(status: StatusCode, title: &str, bar: Option<StatusBar>, message: Markup) -> Self {
        Self {
            status,
            body: page(
                &format!("{title} - zerus"),
                bar,
                html! {
                    h1 { (title) }
                    (message)
                },
            ),
        }
    }
}

impl IntoResponse for ErrorPage {
    fn into_response(self) -> Response {
        (self.status, self.body).into_response()
    }
}

/// Shown when --manifests was not passed
fn no_manifests_page() -> ErrorPage {
    ErrorPage::new(
        StatusCode::NOT_FOUND,
        "No manifests directory",
        None,
        html! {
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

/// The scans and the status bar for a manifest page at `back`, or the page to show instead
/// When the data is stale, this starts a scan, and the page shows the old data until the
/// scan is done.
fn page_scans(state: &AppState, back: &str) -> Result<(Scans, StatusBar), ErrorPage> {
    let browse = state.browse.as_ref().ok_or_else(no_manifests_page)?;
    let now = SystemTime::now();
    let mut scans = browse.scans();
    if browse.is_stale(&scans, now) {
        match browse.start_scan() {
            Ok(ScanStart::Started(_)) => info!("the data is stale, so a page load started a scan"),
            Ok(ScanStart::AlreadyRunning) => {}
            Err(e) => warn!("failed to start a scan: {e}"),
        }
        scans = browse.scans();
    }
    let bar = status_bar(&scans, back, now);
    Ok((scans, bar))
}

/// The snapshot that a manifest page shows, or a page that tells why there is none yet
fn shown(scans: &Scans, bar: &StatusBar) -> Result<Arc<Snapshot>, ErrorPage> {
    scans.shown.clone().ok_or_else(|| {
        ErrorPage::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Scan in progress",
            Some(bar.clone()),
            html! {
                @match scans.running {
                    Some(_) => p {
                        "The first scan of the mirror and the manifests is still running. "
                        "This page reloads when it is done."
                    },
                    None => p { "No scan is done yet. Click Refresh to start one." },
                }
            },
        )
    })
}

fn mirror_scan_failed(bar: &StatusBar) -> ErrorPage {
    ErrorPage::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Mirror scan failed",
        Some(bar.clone()),
        html! {
            p {
                "The last scan could not read the mirror, so this page cannot tell which "
                "crates are in it. The server log gives the cause."
            }
        },
    )
}

fn manifests_unreadable(bar: &StatusBar) -> ErrorPage {
    ErrorPage::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Manifests directory not readable",
        Some(bar.clone()),
        html! {
            p { "The last scan could not read the manifests directory. The server log gives the cause." }
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
async fn manifests_index(State(state): State<Arc<AppState>>) -> Result<Markup, ErrorPage> {
    blocking(move || manifests_index_page(&state)).await
}

fn manifests_index_page(state: &AppState) -> Result<Markup, ErrorPage> {
    let (scans, bar) = page_scans(state, "/")?;
    let snapshot = shown(&scans, &bar)?;
    let Manifests::Files(files) = &snapshot.manifests else {
        return Err(manifests_unreadable(&bar));
    };

    // A crate is only in the mirror because someone put it there, so this page always
    // offers the un-manifested view, even when no manifest file exists yet. It comes first,
    // as it lists the crates of the next transfer.
    Ok(page(
        "manifests - zerus",
        Some(bar),
        html! {
            h1 { "Manifests" }
            table {
                thead {
                    tr {
                        th { "manifest" }
                        th.num { "crates" }
                        th.num { "in mirror" }
                        th.num { "culled" }
                    }
                }
                tbody {
                    tr {
                        td { a href="/unmanifested" { "(un-manifested)" } }
                        @match &snapshot.unmanifested {
                            // Every un-manifested crate is in the mirror, as the mirror is where
                            // the scan found it, so none is culled
                            Ok(crates) => {
                                td.num { (grouped(crates.len())) }
                                td.num { (grouped(crates.len())) }
                                td {}
                            }
                            Err(_) => td.error colspan="3" { "not known" },
                        }
                    }
                    // Newest first, for names that start with the date, as the README shows
                    @for file in files.iter().rev() {
                        tr {
                            td { a href={ "/manifests/" (file.name) } { (file.name) } }
                            @match (&file.crates, snapshot.in_mirror.get(&file.name)) {
                                (Err(_), _) => td.error colspan="3" { "did not parse" },
                                (Ok(crates), Some(&present)) => {
                                    td.num { (grouped(crates.len())) }
                                    td.num { (grouped(present)) }
                                    // Most manifests have no culled crates, so a 0 is only noise
                                    td.num {
                                        @if crates.len() > present {
                                            (grouped(crates.len() - present))
                                        }
                                    }
                                }
                                (Ok(crates), None) => {
                                    td.num { (grouped(crates.len())) }
                                    td.error colspan="2" { "not known" }
                                }
                            }
                        }
                    }
                }
            }
            @if files.is_empty() {
                p.empty { "No manifest files found." }
            }
            @if let Err(why) = snapshot.unmanifested {
                p.error { "The un-manifested count is not known: " (why.reason()) "." }
            }
            (unparsed_warning(snapshot.unparsed()))
        },
    ))
}

#[derive(Serialize)]
struct ScanStatus {
    running: bool,
}

/// Tells a page that waits for a scan if the scan still runs
async fn scan_status(State(state): State<Arc<AppState>>) -> Json<ScanStatus> {
    let running = state
        .browse
        .as_ref()
        .is_some_and(|browse| browse.running().is_some());
    Json(ScanStatus { running })
}

#[derive(serde::Deserialize)]
struct RefreshForm {
    /// The page to go back to after the scan starts
    back: Option<String>,
}

/// Start a scan of the mirror and the manifests, then go back to the page that asked. That
/// page shows the running scan, and reloads until the scan is done.
async fn refresh(State(state): State<Arc<AppState>>, Form(form): Form<RefreshForm>) -> Redirect {
    if let Some(browse) = &state.browse {
        match browse.start_scan() {
            Ok(ScanStart::Started(_)) => info!("refresh started a scan"),
            Ok(ScanStart::AlreadyRunning) => info!("refresh found a scan already running"),
            Err(e) => warn!("failed to start a scan: {e}"),
        }
    }
    Redirect::to(local_path(form.back.as_deref()))
}

/// Detail: the crates listed in one manifest, split into present and culled
async fn manifest_detail(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
    Query(params): Query<ListParams>,
) -> Result<Markup, ErrorPage> {
    blocking(move || manifest_detail_page(&state, &name, params)).await
}

fn manifest_detail_page(
    state: &AppState,
    name: &str,
    params: ListParams,
) -> Result<Markup, ErrorPage> {
    let base = format!("/manifests/{name}");
    let view = params.view(&base);
    let (scans, bar) = page_scans(state, &view.href())?;
    let snapshot = shown(&scans, &bar)?;
    let Manifests::Files(files) = &snapshot.manifests else {
        return Err(manifests_unreadable(&bar));
    };

    // Match against the listing, so a name such as `../secret` cannot reach another file.
    // A bad file elsewhere does not stop this page, only the bad file's own page.
    let found = files.iter().find(|file| file.name == name).ok_or_else(|| {
        ErrorPage::new(
            StatusCode::NOT_FOUND,
            "Manifest not found",
            Some(bar.clone()),
            html! {
                p {
                    "The last scan found no manifest named " code { (name) } ". "
                    "If you added it after that scan, click Refresh."
                }
            },
        )
    })?;
    let crates = found.crates.as_ref().map_err(|e| {
        ErrorPage::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Manifest did not parse",
            Some(bar.clone()),
            html! {
                p { "The manifest " code { (name) } " did not parse:" }
                pre { (format!("{e:#}")) }
                p { "Correct the file, then click Refresh." }
            },
        )
    })?;
    let Mirror::Read(mirror) = &snapshot.mirror else {
        return Err(mirror_scan_failed(&bar));
    };

    let listed = listing(crates, mirror, view.sort);
    let (present, culled) = split_culled(&listed);

    Ok(page(
        &format!("{name} - zerus"),
        Some(bar),
        html! {
            p.crumbs { a href="/" { "manifests" } " /" }
            h1 { (name) }
            p.count {
                (grouped(listed.len())) " crate(s)"
                // The split only tells something when a crate was culled
                @if !culled.is_empty() {
                    ", " (grouped(present.len())) " still in mirror, "
                    (grouped(culled.len())) " culled"
                }
            }
            @match view.show {
                Show::Present => {
                    @if !culled.is_empty() {
                        p { a href=(view.with_show(Show::Culled).href()) {
                            "show culled (" (grouped(culled.len())) ")"
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
                        "show still in mirror (" (grouped(present.len())) ")"
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
) -> Result<Markup, ErrorPage> {
    blocking(move || unmanifested_page(&state, params)).await
}

fn unmanifested_page(state: &AppState, params: ListParams) -> Result<Markup, ErrorPage> {
    // Every entry comes from a file in the mirror, so none of them can be culled.
    let view = ListParams {
        show: None,
        ..params
    }
    .view("/unmanifested");
    let (scans, bar) = page_scans(state, &view.href())?;
    let snapshot = shown(&scans, &bar)?;

    let crates = snapshot.unmanifested.as_ref().map_err(|why| {
        ErrorPage::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Un-manifested crates not known",
            Some(bar.clone()),
            html! {
                p { "This page cannot list the un-manifested crates: " (why.reason()) "." }
            },
        )
    })?;
    // The un-manifested crates are known only after a good mirror scan, so this always matches
    let Mirror::Read(mirror) = &snapshot.mirror else {
        return Err(mirror_scan_failed(&bar));
    };

    let listed = listing(crates, mirror, view.sort);
    let all: Vec<&Listed<'_>> = listed.iter().collect();

    Ok(page(
        "un-manifested - zerus",
        Some(bar),
        html! {
            p.crumbs { a href="/" { "manifests" } " /" }
            h1 { "Un-manifested" }
            p.count { (grouped(all.len())) " crate(s) in the mirror that no manifest records" }
            (unparsed_warning(snapshot.unparsed()))
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
    render: impl FnOnce() -> Result<Markup, ErrorPage> + Send + 'static,
) -> Result<Markup, ErrorPage> {
    tokio::task::spawn_blocking(render)
        .await
        .unwrap_or_else(|e| {
            warn!("page render task failed: {e}");
            Err(ErrorPage::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal error",
                None,
                html! { p { "The page failed to render. The server log gives the cause." } },
            ))
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
) -> Result<Markup, ErrorPage> {
    blocking(move || manifest_search_page(&state, params)).await
}

fn manifest_search_page(
    state: &AppState,
    params: ManifestSearchParams,
) -> Result<Markup, ErrorPage> {
    let query = params.q.unwrap_or_default();
    let (scans, bar) = page_scans(state, &format!("/search?q={}", encode_query(&query)))?;
    let snapshot = shown(&scans, &bar)?;
    let Manifests::Files(files) = &snapshot.manifests else {
        return Err(manifests_unreadable(&bar));
    };

    let needle = query.to_lowercase();
    let parsed = files
        .iter()
        .filter_map(|file| Some((file.name.as_str(), file.crates.as_ref().ok()?)));
    let unparsed = files.iter().filter(|file| file.crates.is_err()).count();

    let mut hits: Vec<(&str, &str, &str)> = Vec::new();
    if !needle.is_empty() {
        for (file, crates) in parsed {
            for c in crates {
                if c.name.to_lowercase().contains(&needle) {
                    hits.push((c.name.as_str(), c.version.as_str(), file));
                }
            }
        }
        hits.sort_unstable();
    }

    Ok(page_with_search(
        "search - zerus",
        &query,
        Focus::Search,
        Some(bar),
        html! {
            h1 { "Search" }
            @if unparsed > 0 {
                p.error { (unparsed) " manifest(s) did not parse, so the search leaves them out." }
            }
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
        .route("/refresh", post(refresh))
        .route("/scan-status", get(scan_status))
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
    rescan: Rescan,
) -> anyhow::Result<()> {
    let state = Arc::new(AppState::new(mirror_path, manifests_path, rescan)?);

    // The first scan runs in the background, so that cargo can use the registry at once
    if let Some(browse) = &state.browse {
        browse
            .start_scan()
            .context("failed to start the first scan")?;
    }

    let app = router(state);
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
        for query in [
            "",
            "sort=zzz&show=all&page=abc",
            "page=-1",
            "page=",
            "sort=Name",
        ] {
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

    /// A mirror holding `serde` and `tokio`, and a manifest that also lists the culled `axum`.
    /// The first scan is done.
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

        let state = AppState::new(mirror, Some(manifests), Rescan::Never).unwrap();
        rescan(&state);
        (tmp, state)
    }

    fn browse(state: &AppState) -> &Arc<Browse> {
        state.browse.as_ref().unwrap()
    }

    fn manifests_dir(state: &AppState) -> PathBuf {
        browse(state).manifests_path.clone()
    }

    /// Scan, wait for the scan to end, and give its result
    fn rescan(state: &AppState) -> Arc<Snapshot> {
        match browse(state).start_scan().unwrap() {
            ScanStart::Started(handle) => handle.join().unwrap(),
            ScanStart::AlreadyRunning => panic!("a scan already runs"),
        }
        browse(state).scans().shown.unwrap()
    }

    /// Wait for a scan that this thread did not start
    fn wait_for_scan(browse: &Browse) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while browse.scans().running.is_some() {
            assert!(Instant::now() < deadline, "the scan did not end");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The fixture without `broken.txt`, scanned again
    fn clean_fixture() -> (tempfile::TempDir, AppState) {
        let (tmp, state) = fixture();
        fs::remove_file(manifests_dir(&state).join("broken.txt")).unwrap();
        rescan(&state);
        (tmp, state)
    }

    /// Write a manifest file and read it into the cache
    fn add_manifest(state: &AppState, name: &str, contents: &str) {
        fs::write(manifests_dir(state).join(name), contents).unwrap();
        rescan(state);
    }

    /// The cell texts of the table row whose link reads `label`
    fn row(html: &str, label: &str) -> Vec<String> {
        let start = html
            .find(&format!(">{label}</a>"))
            .unwrap_or_else(|| panic!("no row {label}: {html}"));
        let start = html[..start].rfind("<tr>").unwrap();
        let end = start + html[start..].find("</tr>").unwrap();
        html[start..end]
            .split("</td>")
            .filter(|cell| cell.contains("<td"))
            .map(|cell| {
                // Drop the tags and keep the text between them
                let mut text = String::new();
                let mut in_tag = false;
                for c in cell.chars() {
                    match c {
                        '<' => in_tag = true,
                        '>' => in_tag = false,
                        _ if !in_tag => text.push(c),
                        _ => {}
                    }
                }
                text
            })
            .collect()
    }

    #[test]
    fn grouped_puts_a_comma_between_groups_of_three() {
        for (n, text) in [
            (0, "0"),
            (999, "999"),
            (1000, "1,000"),
            (123_456, "123,456"),
            (1_234_567, "1,234,567"),
        ] {
            assert_eq!(grouped(n), text);
        }
    }

    /// Model check: for many lengths, the digits stay the same and every group after the
    /// first has three digits
    #[test]
    fn grouped_keeps_the_digits_in_groups_of_three() {
        let samples = (0..20).flat_map(|p| {
            let base = 10usize.pow(p.min(19));
            [base - 1, base, base + 1]
        });
        for n in samples.chain([usize::MAX]) {
            let text = grouped(n);
            assert_eq!(text.replace(',', ""), n.to_string(), "{n}");
            let groups: Vec<&str> = text.split(',').collect();
            assert!((1..=3).contains(&groups[0].len()), "{text}");
            assert!(groups[1..].iter().all(|g| g.len() == 3), "{text}");
        }
    }

    #[test]
    fn manifest_list_marks_the_culled_count_of_each_manifest() {
        let (_tmp, state) = clean_fixture();
        add_manifest(
            &state,
            "b.txt",
            "axum@0.8.1
serde@1.0.210
",
        );

        let list = index(&state).unwrap();

        assert_eq!(row(&list, "b.txt"), ["b.txt", "2", "1", "1"]);
        assert!(list.contains(r#"<th class="num">culled</th>"#), "{list}");
    }

    #[test]
    fn a_manifest_with_nothing_culled_does_not_mention_culled() {
        let (_tmp, state) = clean_fixture();
        add_manifest(&state, "b.txt", "serde@1.0.210\ntokio@1.40.0\n");

        assert_eq!(
            row(&index(&state).unwrap(), "b.txt"),
            ["b.txt", "2", "2", ""]
        );
        let html = detail(&state, "b.txt", "").unwrap();
        assert!(
            html.contains(r#"<p class="count">2 crate(s)</p>"#),
            "{html}"
        );
        // The CSS has a `culled` class, so look at the body only
        let body = html.split("<main>").nth(1).unwrap();
        assert!(!body.contains("culled"), "{body}");
    }

    #[test]
    fn large_counts_are_grouped() {
        let (_tmp, state) = clean_fixture();
        let lines: String = (0..1234).map(|n| format!("gone{n:05}@1.0.0\n")).collect();
        add_manifest(&state, "big.txt", &lines);

        let list = index(&state).unwrap();
        assert_eq!(row(&list, "big.txt"), ["big.txt", "1,234", "0", "1,234"]);
        let html = detail(&state, "big.txt", "").unwrap();
        assert!(
            html.contains("1,234 crate(s), 0 still in mirror, 1,234 culled"),
            "{html}"
        );
    }

    fn render(page: Result<Markup, ErrorPage>) -> Result<String, StatusCode> {
        page.map(Markup::into_string).map_err(|e| e.status)
    }

    fn error_body(page: Result<Markup, ErrorPage>) -> (StatusCode, String) {
        let e = page.unwrap_err();
        (e.status, e.body.into_string())
    }

    fn detail(state: &AppState, name: &str, query: &str) -> Result<String, StatusCode> {
        render(manifest_detail_page(state, name, params(query)))
    }

    fn index(state: &AppState) -> Result<String, StatusCode> {
        render(manifests_index_page(state))
    }

    fn search(state: &AppState, q: &str) -> Result<String, StatusCode> {
        render(manifest_search_page(
            state,
            ManifestSearchParams {
                q: Some(q.to_string()),
            },
        ))
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

        let no_dir = AppState::new(state.mirror_path.clone(), None, Rescan::Never).unwrap();
        let (status, html) = error_body(manifest_detail_page(&no_dir, "a.txt", params("")));
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(html.contains("No manifests directory"), "{html}");

        let gone_dir = AppState::new(
            state.mirror_path.clone(),
            Some(state.mirror_path.join("nope")),
            Rescan::Never,
        )
        .unwrap();
        rescan(&gone_dir);
        let (status, html) = error_body(manifest_detail_page(&gone_dir, "a.txt", params("")));
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(html.contains("Manifests directory not readable"), "{html}");
    }

    #[test]
    fn detail_of_a_bad_manifest_shows_the_parse_error() {
        let (_tmp, state) = fixture();

        let (status, html) = error_body(manifest_detail_page(&state, "broken.txt", params("")));

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            html.contains("broken.txt:1: expected name@version"),
            "{html}"
        );
        assert!(html.contains(r#"action="/refresh""#), "{html}");
    }

    #[test]
    fn detail_of_a_new_manifest_tells_the_user_to_refresh() {
        let (_tmp, state) = fixture();
        fs::write(manifests_dir(&state).join("b.txt"), "serde@1.0.210\n").unwrap();

        let (status, html) = error_body(manifest_detail_page(&state, "b.txt", params("")));

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(html.contains("click Refresh"), "{html}");
        // The refresh comes back to this page
        assert!(
            html.contains(r#"name="back" value="/manifests/b.txt?sort=name""#),
            "{html}"
        );
    }

    #[test]
    fn manifest_pages_use_the_cache_until_a_refresh() {
        let (_tmp, state) = clean_fixture();
        let dir = manifests_dir(&state);
        fs::write(dir.join("b.txt"), "tokio@1.40.0\n").unwrap();
        // Cull `tokio` from the mirror
        let tokio = get_crate_path(&state.mirror_path, "tokio", "1.40.0").unwrap();
        fs::remove_dir_all(&tokio).unwrap();

        // The cache is from before the changes, and a load of the list does not scan
        assert!(!index(&state).unwrap().contains("b.txt"));
        assert_eq!(detail(&state, "b.txt", ""), Err(StatusCode::NOT_FOUND));
        let html = detail(&state, "a.txt", "").unwrap();
        assert!(html.contains("2 still in mirror, 1 culled"), "{html}");

        rescan(&state);

        assert!(index(&state).unwrap().contains("b.txt"));
        let html = detail(&state, "b.txt", "").unwrap();
        assert!(
            html.contains("1 crate(s), 0 still in mirror, 1 culled"),
            "{html}"
        );
        let html = detail(&state, "a.txt", "").unwrap();
        assert!(html.contains("1 still in mirror, 2 culled"), "{html}");
    }

    #[test]
    fn pages_wait_for_the_first_scan() {
        let (tmp, _) = fixture();
        let state = AppState::new(
            tmp.path().join("mirror"),
            Some(tmp.path().join("manifests")),
            Rescan::Never,
        )
        .unwrap();
        *browse(&state).running() = Some(SystemTime::now());

        let (status, html) = error_body(manifests_index_page(&state));
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(html.contains("still running"), "{html}");
        assert!(html.contains("scan running since"), "{html}");
        assert!(
            html.contains(r#"<noscript><meta http-equiv="refresh" content="15"></noscript>"#),
            "{html}"
        );
        assert!(html.contains("fetch('/scan-status')"), "{html}");
        assert!(!html.contains(r#"action="/refresh""#), "{html}");
        for page in [
            detail(&state, "a.txt", ""),
            render(unmanifested_page(&state, params(""))),
            search(&state, "serde"),
        ] {
            assert_eq!(page, Err(StatusCode::SERVICE_UNAVAILABLE));
        }

        *browse(&state).running() = None;
        rescan(&state);
        let list = index(&state).unwrap();
        assert!(!list.contains("http-equiv"), "{list}");
        assert!(!list.contains("/scan-status"), "{list}");
    }

    #[test]
    fn with_no_scan_at_all_the_page_offers_refresh() {
        let (tmp, _) = fixture();
        let state = AppState::new(
            tmp.path().join("mirror"),
            Some(tmp.path().join("manifests")),
            Rescan::Never,
        )
        .unwrap();

        let (status, html) = error_body(manifests_index_page(&state));

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(html.contains("Click Refresh to start one"), "{html}");
        assert!(html.contains(r#"action="/refresh""#), "{html}");
        assert!(!html.contains("http-equiv"), "{html}");
    }

    #[test]
    fn no_manifests_dir_needs_no_scan() {
        let (tmp, _) = fixture();
        let state = AppState::new(tmp.path().join("mirror"), None, Rescan::Never).unwrap();

        assert!(state.browse.is_none());
        let (status, html) = error_body(manifests_index_page(&state));
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(html.contains("No manifests directory"), "{html}");
        assert!(!html.contains(r#"class="status""#), "{html}");
    }

    #[test]
    fn each_scan_gives_a_new_snapshot() {
        let (_tmp, state) = fixture();
        let first = browse(&state).scans().shown.unwrap();

        let second = rescan(&state);

        assert!(!Arc::ptr_eq(&first, &second));
        assert!(browse(&state).scans().running.is_none());
    }

    #[test]
    fn only_one_scan_runs_at_a_time() {
        let (_tmp, state) = fixture();
        let since = SystemTime::now();
        *browse(&state).running() = Some(since);

        assert!(matches!(
            browse(&state).start_scan().unwrap(),
            ScanStart::AlreadyRunning
        ));
        // The running scan keeps its start time
        assert_eq!(browse(&state).scans().running, Some(since));
    }

    #[test]
    fn a_scan_that_panics_still_lets_the_next_scan_start() {
        let (_tmp, state) = fixture();
        *browse(&state).running() = Some(SystemTime::now());

        let done = ScanDone(Arc::clone(browse(&state)));
        let result = std::thread::spawn(move || {
            let _done = done;
            panic!("scan failed");
        })
        .join();

        assert!(result.is_err());
        assert!(browse(&state).scans().running.is_none());
        rescan(&state);
    }

    #[test]
    fn concurrent_starts_never_run_two_scans() {
        let (_tmp, state) = fixture();
        let browse = Arc::clone(browse(&state));

        let starts: Vec<ScanStart> = (0..8)
            .map(|_| {
                let browse = Arc::clone(&browse);
                std::thread::spawn(move || browse.start_scan().unwrap())
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect();

        // A scan can end before the next start, so more than one can start, one after another
        let started = starts
            .into_iter()
            .filter_map(|start| match start {
                ScanStart::Started(handle) => Some(handle),
                ScanStart::AlreadyRunning => None,
            })
            .collect::<Vec<_>>();
        assert!(!started.is_empty());
        for handle in started {
            handle.join().unwrap();
        }
        assert!(browse.scans().running.is_none());
    }

    #[tokio::test]
    async fn refresh_starts_a_scan_and_goes_back_at_once() {
        let (_tmp, state) = clean_fixture();
        fs::write(manifests_dir(&state).join("b.txt"), "serde@1.0.210\n").unwrap();
        let state = Arc::new(state);

        let response = refresh(
            State(Arc::clone(&state)),
            Form(RefreshForm {
                back: Some("/manifests/b.txt?sort=written".to_string()),
            }),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers()["location"],
            "/manifests/b.txt?sort=written"
        );
        wait_for_scan(browse(&state));
        assert!(detail(&state, "b.txt", "").is_ok());
    }

    #[tokio::test]
    async fn refresh_without_a_manifests_dir_goes_back() {
        let (tmp, _) = fixture();
        let state =
            Arc::new(AppState::new(tmp.path().join("mirror"), None, Rescan::Never).unwrap());

        let response = refresh(State(state), Form(RefreshForm { back: None }))
            .await
            .into_response();

        assert_eq!(response.headers()["location"], "/");
    }

    #[test]
    fn rescan_parses_durations_and_zero() {
        for (text, rescan) in [
            ("0", Rescan::Never),
            ("0m", Rescan::Never),
            ("30s", Rescan::After(Duration::from_secs(30))),
            ("10m", Rescan::After(Duration::from_secs(600))),
            ("2h", Rescan::After(Duration::from_secs(7200))),
        ] {
            assert_eq!(text.parse::<Rescan>(), Ok(rescan), "{text}");
        }
    }

    #[test]
    fn rescan_rejects_other_text() {
        for text in [
            "",
            "10",
            "m",
            "-1m",
            "1.5m",
            "10 m",
            "10min",
            "1d",
            "é",
            "1é",
            "99999999999999999999h",
            "18446744073709551615h",
        ] {
            let err = text.parse::<Rescan>().unwrap_err();
            assert!(err.contains("such as 30s, 10m, or 1h"), "{text}: {err}");
        }
    }

    #[test]
    fn data_is_stale_only_after_the_limit_and_with_no_scan_running() {
        let (tmp, _) = fixture();
        let browse = Browse::new(
            tmp.path().join("mirror"),
            tmp.path().join("manifests"),
            Rescan::After(Duration::from_secs(600)),
        )
        .unwrap();
        let shown = fixture().1.browse.unwrap().scans().shown;
        let started = shown.as_ref().unwrap().started;
        let at = |secs| started + Duration::from_secs(secs);
        let scans = |running| Scans {
            shown: shown.clone(),
            running,
        };

        assert!(!browse.is_stale(&scans(None), at(599)));
        assert!(browse.is_stale(&scans(None), at(600)));
        assert!(!browse.is_stale(&scans(Some(at(0))), at(6000)));
        // No data at all, and no scan to bring it
        let empty = Scans {
            shown: None,
            running: None,
        };
        assert!(browse.is_stale(&empty, at(0)));

        let never = Browse::new(
            tmp.path().join("mirror"),
            tmp.path().join("manifests"),
            Rescan::Never,
        )
        .unwrap();
        assert!(!never.is_stale(&scans(None), at(1_000_000)));
        assert!(!never.is_stale(&empty, at(0)));
    }

    #[test]
    fn a_page_load_with_stale_data_starts_a_scan_and_shows_the_old_data() {
        let (tmp, _) = clean_fixture();
        let state = AppState::new(
            tmp.path().join("mirror"),
            Some(tmp.path().join("manifests")),
            Rescan::After(Duration::from_secs(1)),
        )
        .unwrap();
        let old = rescan(&state);
        fs::write(manifests_dir(&state).join("b.txt"), "serde@1.0.210\n").unwrap();
        std::thread::sleep(Duration::from_millis(1100));

        let list = index(&state).unwrap();

        // The page did not wait for the scan
        assert!(!list.contains("b.txt"), "{list}");
        wait_for_scan(browse(&state));
        let new = browse(&state).scans().shown.unwrap();
        assert!(!Arc::ptr_eq(&old, &new));
        assert!(index(&state).unwrap().contains("b.txt"));
    }

    #[test]
    fn a_page_load_with_fresh_data_starts_no_scan() {
        let (tmp, _) = clean_fixture();
        let state = AppState::new(
            tmp.path().join("mirror"),
            Some(tmp.path().join("manifests")),
            Rescan::After(Duration::from_secs(3600)),
        )
        .unwrap();
        let old = rescan(&state);

        index(&state).unwrap();

        assert!(browse(&state).scans().running.is_none());
        assert!(Arc::ptr_eq(&old, &browse(&state).scans().shown.unwrap()));
    }

    #[tokio::test]
    async fn scan_status_tells_if_a_scan_runs() {
        let (_tmp, state) = fixture();
        let state = Arc::new(state);

        assert!(!scan_status(State(Arc::clone(&state))).await.0.running);
        *browse(&state).running() = Some(SystemTime::now());
        assert!(scan_status(State(Arc::clone(&state))).await.0.running);

        let (tmp, _) = fixture();
        let no_dir =
            Arc::new(AppState::new(tmp.path().join("mirror"), None, Rescan::Never).unwrap());
        assert!(!scan_status(State(no_dir)).await.0.running);
    }

    #[test]
    fn ago_uses_the_largest_unit_that_fits() {
        let then = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        for (secs, text) in [
            (0, "0 s ago"),
            (59, "59 s ago"),
            (60, "1 min ago"),
            (3599, "59 min ago"),
            (3600, "1 h ago"),
            (86_399, "23 h ago"),
            (86_400, "1 d ago"),
            (10 * 86_400, "10 d ago"),
        ] {
            assert_eq!(ago(then, then + Duration::from_secs(secs)), text, "{secs}");
        }
        // A clock that went back gives no negative age
        assert_eq!(ago(then, then - Duration::from_secs(5)), "0 s ago");
    }

    #[test]
    fn status_bar_offers_refresh_only_when_no_scan_runs() {
        let (_tmp, state) = fixture();
        let shown = browse(&state).scans().shown;
        let now = shown.as_ref().unwrap().started + Duration::from_secs(180);

        let idle = status_bar(
            &Scans {
                shown: shown.clone(),
                running: None,
            },
            "/x",
            now,
        );
        let html = idle.markup.into_string();
        assert_eq!(idle.reload, Reload::Never);
        assert!(html.contains("data from 3 min ago"), "{html}");
        assert!(html.contains(r#"title="The scan started "#), "{html}");
        assert!(html.contains(r#"name="back" value="/x""#), "{html}");

        let busy = status_bar(
            &Scans {
                shown,
                running: Some(now),
            },
            "/x",
            now,
        );
        let html = busy.markup.into_string();
        assert_eq!(busy.reload, Reload::WhenScanDone);
        assert!(html.contains("data from 3 min ago"), "{html}");
        assert!(html.contains("scan running since"), "{html}");
        assert!(!html.contains("Refresh"), "{html}");
    }

    #[test]
    fn local_path_keeps_paths_on_this_server() {
        for path in ["/", "/manifests/a.txt?sort=name&page=2", "/search?q=a%20b"] {
            assert_eq!(local_path(Some(path)), path);
        }
    }

    #[test]
    fn local_path_rejects_other_sites_and_bad_header_values() {
        for path in [
            "",
            "manifests/a.txt",
            "//evil.example",
            "/\\evil.example",
            "https://evil.example/",
            "/a\r\nSet-Cookie: x=1",
            "/a b",
            "/caf\u{e9}",
        ] {
            assert_eq!(local_path(Some(path)), "/", "{path:?}");
        }
        assert_eq!(local_path(None), "/");
    }

    #[test]
    fn encode_query_round_trips() {
        assert_eq!(encode_query("a b&c=d/é"), "a%20b%26c%3Dd%2F%C3%A9");
        assert_eq!(encode_query("serde-json_1.0~x"), "serde-json_1.0~x");
        // Every encoding is a valid local path to go back to
        for q in ["", "a b", "\r\n", "//", "é"] {
            let back = format!("/search?q={}", encode_query(q));
            assert_eq!(local_path(Some(&back)), back);
        }
    }

    #[test]
    fn every_manifest_page_has_the_status_bar_and_links_back_to_itself() {
        let (_tmp, state) = clean_fixture();

        let list = index(&state).unwrap();
        assert!(list.contains(r#"<div class="status">"#), "{list}");
        assert!(list.contains("data from 0 s ago"), "{list}");
        assert!(list.contains(" UTC and took "), "{list}");
        assert!(list.contains(r#"<form class="refresh" action="/refresh" method="post">"#));
        assert!(list.contains(r#"name="back" value="/""#), "{list}");
        assert_eq!(list.matches("Refresh").count(), 1, "{list}");

        let html = detail(&state, "a.txt", "sort=written").unwrap();
        assert!(
            html.contains(r#"value="/manifests/a.txt?sort=written""#),
            "{html}"
        );
        let html = render(unmanifested_page(&state, params(""))).unwrap();
        assert!(
            html.contains(r#"value="/unmanifested?sort=name""#),
            "{html}"
        );
        let html = search(&state, "a b").unwrap();
        assert!(html.contains(r#"value="/search?q=a%20b""#), "{html}");
    }

    #[test]
    fn header_links_to_the_list_views() {
        let (_tmp, state) = clean_fixture();

        for html in [
            index(&state).unwrap(),
            detail(&state, "a.txt", "").unwrap(),
            search(&state, "").unwrap(),
        ] {
            assert!(html.contains(r#"<a href="/">manifests</a>"#), "{html}");
            assert!(
                html.contains(r#"<a href="/unmanifested">un-manifested</a>"#),
                "{html}"
            );
        }
        let html = detail(&state, "a.txt", "").unwrap();
        assert!(html.contains(r#"<p class="crumbs">"#), "{html}");
    }

    #[test]
    fn manifest_list_puts_unmanifested_first_then_the_newest() {
        let (_tmp, state) = clean_fixture();
        add_manifest(&state, "2026-01-01.txt", "serde@1.0.210\n");
        add_manifest(&state, "2026-02-01.txt", "serde@1.0.210\n");

        let list = index(&state).unwrap();

        let at = |text: &str| list.find(text).unwrap_or_else(|| panic!("{text}: {list}"));
        assert!(at("(un-manifested)") < at("2026-02-01.txt"));
        assert!(at("2026-02-01.txt") < at("2026-01-01.txt"));
    }

    #[test]
    fn scan_time_has_seconds_and_a_zone() {
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_760_000_000);
        assert_eq!(format_time_with_seconds(time), "2025-10-09 08:53:20 UTC");
    }

    #[test]
    fn manifest_list_shows_a_bad_file_and_the_good_ones() {
        let (_tmp, state) = fixture();

        let list = index(&state).unwrap();

        assert!(
            list.contains(r#"<a href="/manifests/a.txt">a.txt</a>"#),
            "{list}"
        );
        assert_eq!(row(&list, "a.txt"), ["a.txt", "3", "2", "1"]);
        assert_eq!(row(&list, "broken.txt"), ["broken.txt", "did not parse"]);
        // `a.txt` records both mirror crates. `broken.txt` does not hide the count.
        assert_eq!(
            row(&list, "(un-manifested)"),
            ["(un-manifested)", "0", "0", ""]
        );
        assert!(
            list.contains("1 manifest(s) did not parse, so the un-manifested crates can include"),
            "{list}"
        );
    }

    #[test]
    fn manifest_list_counts_crates_and_unmanifested() {
        let (_tmp, state) = clean_fixture();
        add_manifest(&state, "a.txt", "serde@1.0.210\naxum@0.8.1\n");

        let list = index(&state).unwrap();

        assert!(
            list.contains(r#"<a href="/manifests/a.txt">a.txt</a>"#),
            "{list}"
        );
        assert_eq!(row(&list, "a.txt"), ["a.txt", "2", "1", "1"]);
        // `tokio` is in the mirror but in no manifest
        assert_eq!(
            row(&list, "(un-manifested)"),
            ["(un-manifested)", "1", "1", ""]
        );
        assert!(!list.contains("not known"), "{list}");
    }

    #[test]
    fn manifest_list_with_an_unreadable_dir_says_so() {
        let (_tmp, state) = fixture();
        fs::remove_dir_all(manifests_dir(&state)).unwrap();
        rescan(&state);

        let (status, html) = error_body(manifests_index_page(&state));

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(html.contains("Manifests directory not readable"), "{html}");
        assert_eq!(
            search(&state, "serde"),
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        );
    }

    #[test]
    fn search_finds_crates_in_the_cached_manifests() {
        let (_tmp, state) = clean_fixture();

        let html = search(&state, "SER").unwrap();

        assert!(html.contains("<td>serde</td>"), "{html}");
        assert!(!html.contains("<td>tokio</td>"), "{html}");
        assert!(!html.contains("did not parse"), "{html}");
    }

    #[test]
    fn search_leaves_out_a_bad_file_and_says_so() {
        let (_tmp, state) = fixture();

        let html = search(&state, "serde").unwrap();

        assert!(html.contains("<td>serde</td>"), "{html}");
        assert!(
            html.contains("1 manifest(s) did not parse, so the search leaves them out."),
            "{html}"
        );
    }

    #[test]
    fn a_failed_mirror_scan_does_not_show_crates_as_culled() {
        let (tmp, state) = clean_fixture();
        let no_mirror = AppState::new(
            tmp.path().join("nope"),
            Some(manifests_dir(&state)),
            Rescan::Never,
        )
        .unwrap();
        rescan(&no_mirror);

        let (status, html) = error_body(unmanifested_page(&no_mirror, params("")));
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(html.contains("could not read the mirror"), "{html}");

        let (status, html) = error_body(manifest_detail_page(&no_mirror, "a.txt", params("")));
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(html.contains("Mirror scan failed"), "{html}");

        let list = index(&no_mirror).unwrap();
        assert_eq!(
            row(&list, "(un-manifested)"),
            ["(un-manifested)", "not known"]
        );
        // The crate count comes from the manifest alone, so it is still known
        assert_eq!(row(&list, "a.txt"), ["a.txt", "3", "not known"]);
        assert!(list.contains("could not read the mirror"), "{list}");
    }

    #[test]
    fn find_unmanifested_needs_the_mirror_and_the_manifest_list() {
        let mirror = || {
            Mirror::Read(WriteTimes::from([(
                Crate::new("serde".to_string(), "1.0.210".to_string()),
                SystemTime::UNIX_EPOCH,
            )]))
        };
        let file = |crates| ParsedFile {
            name: "a.txt".to_string(),
            crates,
        };

        assert_eq!(
            find_unmanifested(&Mirror::Failed, &Manifests::Files(vec![])),
            Err(UnmanifestedUnknown::MirrorScanFailed)
        );
        assert_eq!(
            find_unmanifested(&mirror(), &Manifests::Unreadable),
            Err(UnmanifestedUnknown::ManifestsUnreadable)
        );
        // A bad file does not hide the list. It records nothing that is known.
        assert_eq!(
            find_unmanifested(
                &mirror(),
                &Manifests::Files(vec![file(Err(anyhow::anyhow!("bad")))])
            )
            .unwrap(),
            [Crate::new("serde".to_string(), "1.0.210".to_string())]
        );
        assert_eq!(
            find_unmanifested(&mirror(), &Manifests::Files(vec![file(Ok(vec![]))])).unwrap(),
            [Crate::new("serde".to_string(), "1.0.210".to_string())]
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
    fn unmanifested_lists_crates_despite_a_bad_manifest() {
        let (_tmp, state) = fixture();
        add_manifest(&state, "a.txt", "serde@1.0.210\n");

        let html = render(unmanifested_page(&state, params(""))).unwrap();

        assert!(html.contains("<td>tokio</td>"), "{html}");
        assert!(!html.contains("<td>serde</td>"), "{html}");
        assert!(html.contains("1 manifest(s) did not parse"), "{html}");
        assert_eq!(row(&index(&state).unwrap(), "(un-manifested)")[1], "1");
    }

    #[test]
    fn unmanifested_ignores_show_culled() {
        let (_tmp, state) = clean_fixture();
        add_manifest(&state, "a.txt", "serde@1.0.210\n");

        let html = render(unmanifested_page(&state, params("show=culled"))).unwrap();

        assert!(html.contains("1 crate(s) in the mirror"));
        assert!(html.contains("<td>tokio</td>"));
        assert!(html.contains(r#"href="/unmanifested?sort=written""#));
    }
}
