use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use guppy::errors::Error::CommandError;
use guppy::MetadataCommand;
use rayon::prelude::*;
use reqwest::blocking::Client;
use reqwest::StatusCode;

use crate::build_std::prepare_build_std;
use crate::git::{clone, pull, write_config_json};
use crate::{get_crate_path, Crate};

/// Download all crate files and put into spots that are expected by cargo from crates.io
fn download_and_save(mirror_path: &Path, vendors: Vec<(String, Vec<Crate>)>) -> anyhow::Result<()> {
    vendors.into_par_iter().for_each(|(workspace, mut crates)| {
        println!("[-] Vendoring: {workspace}");
        let client = Client::builder()
            .user_agent(format!("zerus/{} ({})", env!("CARGO_PKG_VERSION"), env!("CARGO_PKG_REPOSITORY")))
            .build()
            .unwrap();
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
                    println!("[-] Couldn't download {name}-{version}, not hosted on crates.io (this is fine if it's rustc internal library)");
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

/// # Returns
/// `Vec<Workspace, Vec<Crate>>`
fn get_deps(
    workspaces: &[String],
    build_std: &Option<String>,
) -> Option<Vec<(String, Vec<Crate>)>> {
    let mut workspace_list: Vec<String> = workspaces.to_vec();
    if let Some(version) = build_std {
        let build_std_path = prepare_build_std(version)?;
        workspace_list.push(build_std_path);
    }

    let mut ret = vec![];
    for workspace in &workspace_list {
        let mut crates = vec![];
        let mut cmd = MetadataCommand::new();
        cmd.manifest_path(workspace.clone());
        println!("[-] Running `cargo metadata` for {workspace}");
        let package_graph = match cmd.build_graph() {
            Ok(p) => p,
            Err(CommandError(e)) => {
                // most likely: "error: the manifest-path must be a path to a Cargo.toml file"
                println!("[!] Could not run `cargo metadata`: {e:}");
                return None;
            }
            Err(_) => {
                println!("[!] Could not run `cargo metadata`");
                return None;
            }
        };

        for package in package_graph.packages() {
            let c = Crate::new(package.name().to_string(), package.version().to_string());
            if !crates.contains(&c) {
                crates.push(c);
            }
        }
        ret.push((workspace.clone(), crates));
    }

    Some(ret)
}

pub fn mirror(
    mirror_path: PathBuf,
    workspaces: Vec<String>,
    build_std: Option<String>,
    git_index_url: Option<String>,
    git_index: bool,
) {
    std::fs::create_dir_all(&mirror_path).unwrap();
    println!("[-] Created {}", mirror_path.display());

    let Some(crates) = get_deps(&workspaces, &build_std) else {
        return;
    };

    download_and_save(&mirror_path, crates).expect("unable to download crates");
    println!("[-] Finished downloading crates");

    if git_index {
        println!("[-] Syncing git index crates.io");
        let repo = mirror_path.join("crates.io-index");
        if repo.exists() {
            pull(Path::new(&repo)).unwrap();
        } else {
            clone(Path::new(&repo)).unwrap();
        }
        println!("[-] Done syncing git index crates.io");
    }

    if let Some(url) = git_index_url {
        let path = mirror_path.join("crates.io-index").join("config.json");
        let file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)
            .unwrap();
        write_config_json(&url, file).unwrap();
    }
}
