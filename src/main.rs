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
    workspace: Vec<String>,
}

fn main() {
    let args = Args::parse();

    std::fs::create_dir_all(&args.mirror_path).unwrap();
    println!("[-] Created {}", args.mirror_path.display());

    let crates = get_deps(&args);

    download_and_save(&args.mirror_path, crates).expect("unable to download crates");
    println!("[-] Finished Downloading");
}

fn get_deps(args: &Args) -> Vec<Crate> {
    let mut crates = vec![];
    for workspace in &args.workspace {
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
    }

    crates
}

pub fn get_crate_path(
    mirror_path: &Path,
    crate_name: &str,
    crate_version: &str,
) -> Option<PathBuf> {
    let crate_path = match crate_name.len() {
        1 => PathBuf::from("1"),
        2 => PathBuf::from("2"),
        3 => PathBuf::from("3"),
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
fn download_and_save(mirror_path: &Path, vendors: Vec<Crate>) -> anyhow::Result<()> {
    // TODO: async downloading
    for Crate { name, version } in vendors {
        let dir_crate_path = get_crate_path(mirror_path, &name, &version).unwrap();
        let crate_path = dir_crate_path.join(format!("{name}-{version}.crate"));

        // check if file already exists
        if fs::metadata(&crate_path).is_err() {
            // download
            let url = format!("https://static.crates.io/crates/{name}/{name}-{version}.crate");
            println!("[-] Downloading: {url}");
            // TODO: save one client and call get()
            let response = reqwest::blocking::get(url)?.bytes()?;

            fs::create_dir_all(&dir_crate_path)?;
            fs::write(crate_path, response)?;
        }
    }

    Ok(())
}
