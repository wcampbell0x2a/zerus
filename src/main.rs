mod build_std;

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueHint};
use time::macros::format_description;
use time::OffsetDateTime;
use tracing::{debug, error, info};
use tracing_indicatif::IndicatifLayer;
use tracing_subscriber::layer::{Layer, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

mod git;
mod index;
mod manifest;
mod mirror;
mod pack;
mod serve;

fn validate_url(url: &str) -> Result<String, String> {
    if url.starts_with("http://") || url.starts_with("https://") {
        Ok(url.to_string())
    } else {
        Err(String::from("The URL must start with http:// or https://"))
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone)]
struct Crate {
    name: String,
    version: String,
}

impl Crate {
    pub fn new(name: String, version: String) -> Self {
        Self { name, version }
    }
}

#[derive(Parser)]
#[command(version, about)]
struct Args {
    #[command(subcommand)]
    command: Command,

    /// Show the full log of every download/processing line (debug level)
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Create offline mirror of crate files
    Mirror {
        /// new directory to contain offline mirror crate files
        mirror_path: PathBuf,

        /// list of Cargo.toml files to vendor depends
        workspaces: Vec<String>,

        /// Crates to mirror (format: name@version or name for latest, e.g. reqwest@0.12.8 or reqwest).
        /// Implies --get-feature-gated: downloads the full recursive dependency closure
        /// (including dev/build deps, ignoring features), which can be thousands of crates
        #[arg(long = "crate", value_name = "NAME[@VERSION]")]
        extra_crates: Vec<String>,

        /// Cache build-std depends for nightly toolchain (e.g. nightly-2024-10-09)
        #[arg(long, value_name = "VERSION")]
        build_std: Option<String>,

        /// Hostname for git index crates.io
        #[arg(long)]
        #[arg(value_hint = ValueHint::Url, value_parser = validate_url)]
        #[arg(requires = "git_index")]
        git_index_url: Option<String>,

        /// Download git index crates.io
        #[arg(long)]
        git_index: bool,

        /// For each depends, extract and grab all depends. This ignores enabled features
        /// and includes dev/build dependencies, so even a small crate can expand the
        /// mirror to thousands of crates
        #[arg(long)]
        get_feature_gated: bool,
    },
    /// Generate a limited crates git index from .crate files
    UpdateIndex {
        /// Path to mirror directory (contains crates.io-index/ and crates/)
        mirror_path: PathBuf,

        /// Download URL template for config.json
        #[arg(long)]
        #[arg(value_hint = ValueHint::Url, value_parser = validate_url)]
        dl_url: Option<String>,
    },
    /// Write a manifest (name@version per line) of all crates in the mirror
    GenerateManifest {
        /// Path to mirror directory (contains crates/)
        mirror_path: PathBuf,

        /// Output file (defaults to stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Remove crates listed in manifest(s) from the mirror to avoid re-transferring them
    Cull {
        /// Path to mirror directory (contains crates/)
        mirror_path: PathBuf,

        /// Manifest files from previous transfers (union is culled)
        #[arg(required = true)]
        manifests: Vec<PathBuf>,

        /// Only print what would be removed
        #[arg(long)]
        dry_run: bool,
    },
    /// Download, cull, and record a transfer as one pack file
    Pack {
        /// new directory to contain offline mirror crate files
        mirror_path: PathBuf,

        /// list of Cargo.toml files to vendor depends
        workspaces: Vec<String>,

        /// Crates to mirror (format: name@version or name for latest).
        /// Implies --get-feature-gated
        #[arg(long = "crate", value_name = "NAME[@VERSION]")]
        extra_crates: Vec<String>,

        /// Pack file to write (default: transfers/<today>.zpk)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Directory of manifest files from previous transfers. Crates already recorded
        /// there are left out of the pack, and this transfer is recorded there on success.
        /// Required, because a path relative to the current directory would quietly point
        /// at a different record from a different directory
        #[arg(long, value_name = "DIR", required_unless_present = "no_record")]
        manifests: Option<PathBuf>,

        /// Build the pack without recording it, for a transfer that may not happen
        #[arg(long)]
        no_record: bool,

        /// Cache build-std depends for nightly toolchain (e.g. nightly-2024-10-09)
        #[arg(long, value_name = "VERSION")]
        build_std: Option<String>,

        /// For each depends, extract and grab all depends. This ignores enabled features
        /// and includes dev/build dependencies
        #[arg(long)]
        get_feature_gated: bool,
    },
    /// Pack crates already in a mirror, with no download step
    PackFromMirror {
        /// Path to mirror directory (contains crates/)
        mirror_path: PathBuf,

        /// Pack file to write (default: transfers/<today>.zpk)
        output: Option<PathBuf>,

        /// Directory of manifest files from previous transfers. Crates already recorded
        /// there are left out of the pack, and this transfer is recorded there on success
        #[arg(long, value_name = "DIR")]
        manifests: Option<PathBuf>,

        /// Build the pack without recording it
        #[arg(long)]
        no_record: bool,
    },
    /// Merge a pack file into an offline mirror and update the index
    Unpack {
        /// Path to mirror directory (created if missing)
        mirror_path: PathBuf,

        /// Pack file to merge
        pack: PathBuf,

        /// Download URL template for config.json
        #[arg(long)]
        #[arg(value_hint = ValueHint::Url, value_parser = validate_url)]
        dl_url: Option<String>,

        /// Directory to record this transfer in, giving the offline side the same record
        /// the sending side kept. Defaults to `manifests/` beside the mirror
        #[arg(long, value_name = "DIR")]
        manifests: Option<PathBuf>,

        /// Do not record the transfer in the manifests directory
        #[arg(long, conflicts_with = "manifests")]
        no_record: bool,

        /// List what the pack holds without writing to the mirror
        #[arg(long)]
        dry_run: bool,
    },
    /// Serve crate registry with sparse index, downloads, and search
    Serve {
        /// Path to mirror directory
        mirror_path: PathBuf,

        /// Address to bind to
        #[arg(long, default_value = "0.0.0.0:8080")]
        bind: String,

        /// Directory of manifest files from previous transfers, browsable in the web UI
        #[arg(long, value_name = "DIR")]
        manifests: Option<PathBuf>,

        /// Accept pack uploads at POST /admin/upload, authorized by this token.
        /// Uploads merge into the mirror and update the index
        #[arg(long, value_name = "TOKEN", env = "ZERUS_UPLOAD_TOKEN")]
        upload_token: Option<String>,

        /// Download URL template written to config.json when an upload rebuilds the index
        #[arg(long)]
        #[arg(value_hint = ValueHint::Url, value_parser = validate_url)]
        dl_url: Option<String>,
    },
}

fn main() {
    if let Err(e) = run() {
        error!("{e:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let args = Args::parse();

    // `--verbose` raises the default level to `debug`; `RUST_LOG` overrides.
    let default_level = if args.verbose { "debug" } else { "info" };
    // backhand narrates each squashfs section at info level, which buries the pack/unpack
    // progress, so it is quieted unless RUST_LOG asks for it.
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("zerus={default_level},backhand=warn,info")));

    // Deterministic output (no timestamps/ANSI) for snapshot tests.
    let test_log = std::env::var_os("ZERUS_LOG_TEST").is_some();
    let indicatif_layer = IndicatifLayer::new();
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_ansi(!test_log)
        .with_writer(indicatif_layer.get_stderr_writer());

    let fmt_layer = if test_log {
        fmt_layer.without_time().boxed()
    } else {
        fmt_layer.boxed()
    };
    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(indicatif_layer)
        .with(filter)
        .init();

    match args.command {
        Command::Mirror {
            mirror_path,
            workspaces,
            extra_crates,
            build_std,
            git_index_url,
            git_index,
            get_feature_gated,
        } => {
            if workspaces.is_empty() && extra_crates.is_empty() && build_std.is_none() {
                anyhow::bail!("provide at least one workspace, --crate, or --build-std");
            }
            mirror::mirror(
                mirror_path,
                workspaces,
                extra_crates,
                build_std,
                git_index_url,
                git_index,
                get_feature_gated,
            )?;
        }
        Command::UpdateIndex {
            mirror_path,
            dl_url,
        } => {
            let index_path = mirror_path.join("crates.io-index");
            let crates_path = mirror_path.join("crates");
            index::update_index(&index_path, &crates_path, dl_url.as_deref())?;
        }
        Command::GenerateManifest {
            mirror_path,
            output,
        } => {
            let crates = manifest::generate(&mirror_path)?;
            manifest::write_manifest(&crates, output.as_deref())?;
        }
        Command::Cull {
            mirror_path,
            manifests,
            dry_run,
        } => {
            manifest::cull(&mirror_path, &manifests, dry_run)?;
        }
        Command::Pack {
            mirror_path,
            workspaces,
            extra_crates,
            output,
            manifests,
            no_record,
            build_std,
            get_feature_gated,
        } => {
            if workspaces.is_empty() && extra_crates.is_empty() && build_std.is_none() {
                anyhow::bail!("provide at least one workspace, --crate, or --build-std");
            }
            pack_transfer(PackTransfer {
                mirror_path,
                workspaces,
                extra_crates,
                output: output.unwrap_or_else(default_output),
                manifests,
                no_record,
                build_std,
                get_feature_gated,
            })?;
        }
        Command::PackFromMirror {
            mirror_path,
            output,
            manifests,
            no_record,
        } => {
            let output = output.unwrap_or_else(default_output);
            write_pack(&mirror_path, &output, manifests.as_deref(), no_record)?;
        }
        Command::Unpack {
            mirror_path,
            pack,
            dl_url,
            manifests,
            no_record,
            dry_run,
        } => {
            // The record lives beside the mirror by default, so an unpack with no extra
            // flags still leaves the offline side knowing what arrived.
            let manifests = if no_record {
                None
            } else {
                Some(manifests.unwrap_or_else(|| mirror_path.join("manifests")))
            };
            unpack_transfer(UnpackTransfer {
                mirror_path,
                pack,
                dl_url,
                manifests,
                dry_run,
            })?;
        }
        Command::Serve {
            mirror_path,
            bind,
            manifests,
            upload_token,
            dl_url,
        } => {
            serve::serve(serve::Config {
                mirror_path,
                bind,
                manifests_path: manifests,
                upload_token,
                dl_url,
            })?;
        }
    }

    Ok(())
}

/// Inputs for one `pack` run
struct PackTransfer {
    mirror_path: PathBuf,
    workspaces: Vec<String>,
    extra_crates: Vec<String>,
    output: PathBuf,
    manifests: Option<PathBuf>,
    no_record: bool,
    build_std: Option<String>,
    get_feature_gated: bool,
}

/// Download, select what has not gone yet, write the pack, then record the transfer.
///
/// This replaces the `mirror` -> `cull` -> `generate-manifest` sequence. Culling used to
/// delete already-transferred crates from the mirror; here the mirror keeps everything and
/// the pack carries only what is new, so the local mirror stays whole between transfers.
fn pack_transfer(t: PackTransfer) -> anyhow::Result<()> {
    mirror::mirror(
        t.mirror_path.clone(),
        t.workspaces,
        t.extra_crates,
        t.build_std,
        None,
        false,
        t.get_feature_gated,
    )?;

    write_pack(&t.mirror_path, &t.output, t.manifests.as_deref(), t.no_record)
}

/// Select what has not been transferred, write the pack, and record it.
///
/// `manifests` of `None` packs the whole mirror and records nothing, for a one-off pack of
/// a mirror that is not part of a repeating transfer.
fn write_pack(
    mirror_path: &Path,
    output: &Path,
    manifests: Option<&Path>,
    no_record: bool,
) -> anyhow::Result<()> {
    let total = pack::count_crates(mirror_path);
    let crates = pack::pending(mirror_path, manifests)?;
    if crates.is_empty() {
        match manifests {
            Some(dir) => info!(
                "mirror holds {total} crate(s), all recorded in {}; nothing new to pack",
                dir.display()
            ),
            None => info!("mirror holds no crates; nothing to pack"),
        }
        return Ok(());
    }

    info!(
        "packing {} of {total} crate(s); {} already recorded",
        crates.len(),
        total - crates.len()
    );
    let summary = pack::write(mirror_path, &crates, output)?;
    info!(
        "wrote {} ({} crate(s), {})",
        output.display(),
        summary.crates.len(),
        human_bytes(summary.bytes)
    );

    match manifests {
        Some(dir) if !no_record => {
            let path = pack::record(dir, &pack::manifest_name(output), &summary.crates)?;
            info!("recorded this transfer in {}", path.display());
        }
        Some(_) => info!("not recorded (--no-record); re-running will pack these crates again"),
        None => {}
    }

    Ok(())
}

/// Where a pack goes when the command line does not say.
///
/// Transfers are dated, one per day in the common case, so today's date names the file. A
/// second pack on the same day would overwrite the first, so the name gains a counter.
fn default_output() -> PathBuf {
    let today = OffsetDateTime::now_utc()
        .date()
        .format(format_description!("[year]-[month]-[day]"))
        .unwrap_or_else(|_| String::from("transfer"));

    let dir = Path::new("transfers");
    let first = dir.join(format!("{today}.{PACK_EXTENSION}"));
    if !first.exists() {
        return first;
    }

    // Bounded so a wedged loop cannot spin; two-a-day is already unusual.
    (2..100)
        .map(|n| dir.join(format!("{today}-{n}.{PACK_EXTENSION}")))
        .find(|path| !path.exists())
        .unwrap_or(first)
}

/// Conventional extension for a pack file
const PACK_EXTENSION: &str = "zpk";

/// Inputs for one `unpack` run
struct UnpackTransfer {
    mirror_path: PathBuf,
    pack: PathBuf,
    dl_url: Option<String>,
    /// Where to record the transfer, or `None` for `--no-record`
    manifests: Option<PathBuf>,
    dry_run: bool,
}

/// Merge a pack into the mirror, rebuild the index, and record what arrived
fn unpack_transfer(t: UnpackTransfer) -> anyhow::Result<()> {
    if t.dry_run {
        let crates = pack::read_manifest(&t.pack)?;
        info!("{} holds {} crate(s):", t.pack.display(), crates.len());
        for c in &crates {
            info!("  {}@{}", c.name, c.version);
        }
        return Ok(());
    }

    let summary = pack::merge(&t.pack, &t.mirror_path)?;

    // Recorded even when the merge added nothing: the pack still made the trip, and the
    // record is of the transfer, not of what happened to be missing.
    if let Some(dir) = &t.manifests {
        let name = t
            .pack
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| String::from("transfer"));
        let path = pack::record_merge(dir, &name, &summary)?;
        info!("recorded this transfer in {}", path.display());
    }

    if !summary.changed() {
        info!("mirror already held every crate in the pack; index left alone");
        return Ok(());
    }

    for c in &summary.added {
        debug!("added {}@{}", c.name, c.version);
    }

    index::update_index(
        &t.mirror_path.join("crates.io-index"),
        &t.mirror_path.join("crates"),
        t.dl_url.as_deref(),
    )?;

    info!(
        "added {} crate(s), skipped {} already present; mirror now holds {}",
        summary.added.len(),
        summary.skipped.len(),
        pack::count_crates(&t.mirror_path)
    );

    Ok(())
}

/// Size in the largest unit that keeps the number readable
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// See https://doc.rust-lang.org/cargo/reference/registries.html#index-format
///
/// Returns the prefix path component used by both crate storage and index files.
pub fn get_index_prefix(crate_name: &str) -> Option<PathBuf> {
    match crate_name.len() {
        1 => Some(PathBuf::from("1")),
        2 => Some(PathBuf::from("2")),
        3 => {
            let first = crate_name.get(0..1)?;
            Some([PathBuf::from("3"), first.into()].iter().collect())
        }
        n if n >= 4 => {
            let first_two = crate_name.get(0..2)?;
            let second_two = crate_name.get(2..4)?;
            Some([first_two, second_two].iter().collect())
        }
        _ => None,
    }
}

/// See https://doc.rust-lang.org/cargo/reference/registries.html#index-format
///
/// This follows the following config.json:
/// ```json
/// {
///   "dl": "http://[IP]/crates/{prefix}/{crate}/{version}/{crate}-{version}.crate",
///   "api": "http://[IP]/crates"
/// }
/// ```
pub fn get_crate_path(
    mirror_path: &Path,
    crate_name: &str,
    crate_version: &str,
) -> Option<PathBuf> {
    let crate_path = get_index_prefix(crate_name)?;

    Some(
        mirror_path
            .join("crates")
            .join(crate_path)
            .join(crate_name)
            .join(crate_version),
    )
}
