mod build_std;
use build_std::prepare_build_std;
use git::write_config_json;
use guppy::errors::Error::CommandError;
use reqwest::StatusCode;

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::{fs, iter};

use clap::{Parser, ValueHint};
use guppy::graph::DependencyDirection;
use guppy::MetadataCommand;
use rayon::prelude::*;
use reqwest::blocking::Client;

use crate::git::{clone, pull};

mod git;

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
    /// new directory to contain offline mirror crate files
    mirror_path: PathBuf,

    /// list of Cargo.toml files to vendor depends
    workspaces: Vec<String>,

    /// Cache build-std depends for nightly version
    #[arg(long, value_name = "VERSION")]
    build_std: Option<String>,

    /// Hostname for git index crates.io
    #[arg(long)]
    #[arg(value_hint = ValueHint::Url, value_parser = validate_url)]
    #[arg(conflicts_with = "skip_git_index")]
    git_index_url: Option<String>,

    /// Skip download of git index crates.io
    #[arg(long)]
    #[arg(conflicts_with = "git_index_url")]
    skip_git_index: bool,
}

fn main() {
    let args = Args::parse();

    std::fs::create_dir_all(&args.mirror_path).unwrap();
    println!("[-] Created {}", args.mirror_path.display());

    let Some(crates) = get_deps(&args) else {
        return;
    };

    download_and_save(&args.mirror_path, crates).expect("unable to download crates");
    println!("[-] Finished downloading crates");

    if !args.skip_git_index {
        println!("[-] Syncing git index crates.io");
        let repo = args.mirror_path.join("crates.io-index");
        if repo.exists() {
            pull(Path::new(&repo)).unwrap();
        } else {
            clone(Path::new(&repo)).unwrap();
        }
        println!("[-] Done syncing git index crates.io");
    }

    if let Some(url) = args.git_index_url {
        let path = args.mirror_path.join("crates.io-index").join("config.json");
        let file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)
            .unwrap();
        write_config_json(&url, file).unwrap();
    }
}

/// # Returns
/// `Vec<Workspace, Vec<Crate>>`
fn get_deps(args: &Args) -> Option<Vec<(String, Vec<Crate>)>> {
    let mut workspaces = args.workspaces.clone();
    if let Some(version) = &args.build_std {
        let build_std = prepare_build_std(version)?;
        workspaces.push(build_std);
    }

    let mut ret = vec![];
    for workspace in workspaces {
        let mut crates = vec![];
        let package_graph = match MetadataCommand::new()
            .manifest_path(workspace.clone())
            .build_graph()
        {
            Ok(p) => p,
            Err(CommandError(e)) => {
                if args.build_std.is_some() {
                    println!("[!] Could not run `cargo metadata`, try `rusutp default nightly` during zerus invocation, or set $CARGO to `cargo +nightly` location");
                } else {
                    // most likely: "error: the manifest-path must be a path to a Cargo.toml file"
                    println!("[!] Could not run `cargo metadata`: {e:}");
                }
                return None;
            }
            Err(_) => {
                println!("[!] Could not run `cargo metadata`");
                return None;
            }
        };

        let packages = package_graph.packages();
        for package in packages {
            let id = package.id();
            let query = package_graph.query_forward(iter::once(id)).unwrap();
            let package_set = query.resolve();
            for package in package_set.packages(DependencyDirection::Forward) {
                let c = Crate::new(package.name().to_string(), package.version().to_string());
                if !crates.contains(&c) {
                    crates.push(c);
                }
            }
        }
        ret.push((workspace.clone(), crates));
    }

    Some(ret)
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
    let crate_path = match crate_name.len() {
        1 => PathBuf::from("1"),
        2 => PathBuf::from("2"),
        3 => {
            let first = crate_name.get(0..1)?;
            [PathBuf::from("3"), first.into()].iter().collect()
        }
        n if n >= 4 => {
            let first_two = crate_name.get(0..2)?;
            let second_two = crate_name.get(2..4)?;
            [first_two, second_two].iter().collect()
        }
        _ => return None,
    };

    Some(
        mirror_path
            .join("crates")
            .join(crate_path)
            .join(crate_name)
            .join(crate_version),
    )
}

/// Download all crate files and put into spots that are expected by cargo from crates.io
fn download_and_save(mirror_path: &Path, vendors: Vec<(String, Vec<Crate>)>) -> anyhow::Result<()> {
    vendors.into_par_iter().for_each(|(workspace, mut crates)| {
        println!("[-] Vendoring: {workspace}");
        let client = Client::new();
        crates.sort();

        crates.into_par_iter().for_each(|c| {
            let Crate { name, version } = c;
            let dir_crate_path = get_crate_path(mirror_path, &name, &version).unwrap();
            let crate_path = dir_crate_path.join(format!("{name}-{version}.crate"));

            // check if file already exists
            if !fs::exists(&crate_path).unwrap() {
                // download
                let url = format!("https://static.crates.io/crates/{name}/{name}-{version}.crate");
                println!("[-] Downloading: {url}");
                let Ok(response) = client.get(url).send() else {
                    return;
                };

                if response.status() != StatusCode::OK {
                    println!("[-] Couldn't download {name}-{version}, not hosted on crates.io");
                    return;
                }

                let Ok(response) = response.bytes() else {
                    return;
                };

                fs::create_dir_all(&dir_crate_path).unwrap();
                fs::write(crate_path.clone(), response).unwrap();
            }
        })
    });

    Ok(())
}
