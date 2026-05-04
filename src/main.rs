mod build_std;

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueHint};

mod git;
mod index;
mod mirror;

fn validate_url(url: &str) -> Result<String, String> {
    if url.starts_with("http://") || url.starts_with("https://") {
        Ok(url.to_string())
    } else {
        Err(String::from("The URL must start with http:// or https://"))
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug)]
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
}

#[derive(Subcommand)]
enum Command {
    /// Create offline mirror of crate files
    Mirror {
        /// new directory to contain offline mirror crate files
        mirror_path: PathBuf,

        /// list of Cargo.toml files to vendor depends
        workspaces: Vec<String>,

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
}

fn main() {
    let args = Args::parse();

    match args.command {
        Command::Mirror {
            mirror_path,
            workspaces,
            build_std,
            git_index_url,
            git_index,
        } => {
            mirror::mirror(mirror_path, workspaces, build_std, git_index_url, git_index);
        }
        Command::UpdateIndex {
            mirror_path,
            dl_url,
        } => {
            let index_path = mirror_path.join("crates.io-index");
            let crates_path = mirror_path.join("crates");
            index::update_index(&index_path, &crates_path, dl_url.as_deref());
        }
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
