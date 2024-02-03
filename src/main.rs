mod build_std;
use build_std::prepare_build_std;
use reqwest::blocking::Client;

use std::path::{Path, PathBuf};
use std::{fs, iter};

use clap::Parser;
use guppy::graph::DependencyDirection;
use guppy::MetadataCommand;

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
struct Args {
    /// new directory to contain offline mirror crate files
    mirror_path: PathBuf,

    /// list of Cargo.toml files to vendor depends
    workspaces: Vec<String>,

    /// Cache build-std depends for nightly version
    #[clap(long, value_name = "VERSION")]
    build_std: Option<String>,
}

fn main() {
    let args = Args::parse();

    std::fs::create_dir_all(&args.mirror_path).unwrap();
    println!("[-] Created {}", args.mirror_path.display());

    let Some(crates) = get_deps(&args) else {
        return;
    };

    download_and_save(&args.mirror_path, crates).expect("unable to download crates");
    println!("[-] Finished Downloading");
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
        let package_graph = MetadataCommand::new()
            .manifest_path(workspace.clone())
            .build_graph()
            .unwrap();

        let packages = package_graph.packages();
        for package in packages {
            let id = package.id();
            let query = package_graph.query_forward(iter::once(id)).unwrap();
            let package_set = query.resolve();
            for package in package_set.packages(DependencyDirection::Forward) {
                crates.push(Crate::new(
                    package.name().to_string(),
                    package.version().to_string(),
                ));
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
    // TODO: async downloading
    let client = Client::new();
    for (workspace, crates) in vendors {
        println!("[-] Vendoring: {workspace}");
        for Crate { name, version } in crates {
            let dir_crate_path = get_crate_path(mirror_path, &name, &version).unwrap();
            let crate_path = dir_crate_path.join(format!("{name}-{version}.crate"));

            // check if file already exists
            if fs::metadata(&crate_path).is_err() {
                // download
                let url = format!("https://static.crates.io/crates/{name}/{name}-{version}.crate");
                println!("[-] Downloading: {url}");
                let response = client.get(url).send()?.bytes()?;

                fs::create_dir_all(&dir_crate_path)?;
                fs::write(crate_path, response)?;
            }
        }
    }

    Ok(())
}
