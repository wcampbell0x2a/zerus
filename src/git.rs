use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};

use git2::build::{CheckoutBuilder, RepoBuilder};
use git2::{AutotagOption, FetchOptions, Progress, RemoteCallbacks, RemoteUpdateFlags, Repository};

struct State {
    progress: Option<Progress<'static>>,
    total: usize,
    current: usize,
    path: Option<PathBuf>,
    newline: bool,
}

fn print(state: &mut State) {
    let stats = state.progress.as_ref().unwrap();
    let network_pct = (100 * stats.received_objects()) / stats.total_objects();
    let index_pct = (100 * stats.indexed_objects()) / stats.total_objects();
    let co_pct = if state.total > 0 {
        (100 * state.current) / state.total
    } else {
        0
    };
    let kbytes = stats.received_bytes() / 1024;
    if stats.received_objects() == stats.total_objects() {
        if !state.newline {
            println!();
            state.newline = true;
        }
        print!(
            "Resolving deltas {}/{}\r",
            stats.indexed_deltas(),
            stats.total_deltas()
        );
    } else {
        print!(
            "net {:3}% ({:4} kb, {:5}/{:5})  /  idx {:3}% ({:5}/{:5})  \
             /  chk {:3}% ({:4}/{:4}) {}\r",
            network_pct,
            kbytes,
            stats.received_objects(),
            stats.total_objects(),
            index_pct,
            stats.indexed_objects(),
            stats.total_objects(),
            co_pct,
            state.current,
            state.total,
            state
                .path
                .as_ref()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        )
    }
    std::io::stdout().flush().unwrap();
}

pub fn clone(repo_path: &Path) -> Result<(), git2::Error> {
    let state = RefCell::new(State {
        progress: None,
        total: 0,
        current: 0,
        path: None,
        newline: false,
    });
    let mut cb = RemoteCallbacks::new();
    cb.transfer_progress(|stats| {
        let mut state = state.borrow_mut();
        state.progress = Some(stats.to_owned());
        print(&mut state);
        true
    });

    let mut co = CheckoutBuilder::new();
    co.progress(|path, cur, total| {
        let mut state = state.borrow_mut();
        state.path = path.map(|p| p.to_path_buf());
        state.current = cur;
        state.total = total;
        print(&mut state);
    });

    let mut fo = FetchOptions::new();
    fo.remote_callbacks(cb);
    RepoBuilder::new()
        .fetch_options(fo)
        .with_checkout(co)
        .clone("https://github.com/rust-lang/crates.io-index", repo_path)?;
    println!("done");

    Ok(())
}

pub fn pull(repo: &Path) -> Result<(), git2::Error> {
    let repo = Repository::open(repo)?;
    let remote = "origin";

    // Figure out whether it's a named remote or a URL
    println!("Fetching {} for repo", remote);
    let mut cb = RemoteCallbacks::new();
    let mut remote = repo
        .find_remote(remote)
        .or_else(|_| repo.remote_anonymous(remote))?;
    cb.sideband_progress(|data| {
        print!("remote: {}", std::str::from_utf8(data).unwrap());
        std::io::stdout().flush().unwrap();
        true
    });

    // This callback gets called for each remote-tracking branch that gets
    // updated. The message we output depends on whether it's a new one or an
    // update.
    cb.update_tips(|refname, a, b| {
        if a.is_zero() {
            println!("[new]     {:20} {}", b, refname);
        } else {
            println!("[updated] {:10}..{:10} {}", a, b, refname);
        }
        true
    });

    // Here we show processed and total objects in the pack and the amount of
    // received data. Most frontends will probably want to show a percentage and
    // the download rate.
    cb.transfer_progress(|stats| {
        if stats.received_objects() == stats.total_objects() {
            print!(
                "Resolving deltas {}/{}\r",
                stats.indexed_deltas(),
                stats.total_deltas()
            );
        } else if stats.total_objects() > 0 {
            print!(
                "Received {}/{} objects ({}) in {} bytes\r",
                stats.received_objects(),
                stats.total_objects(),
                stats.indexed_objects(),
                stats.received_bytes()
            );
        }
        std::io::stdout().flush().unwrap();
        true
    });

    // Download the packfile and index it. This function updates the amount of
    // received data and the indexer stats which lets you inform the user about
    // progress.
    let mut fo = FetchOptions::new();
    fo.remote_callbacks(cb);
    remote.download(&[] as &[&str], Some(&mut fo))?;

    {
        // If there are local objects (we got a thin pack), then tell the user
        // how many objects we saved from having to cross the network.
        let stats = remote.stats();
        if stats.local_objects() > 0 {
            println!(
                "\rReceived {}/{} objects in {} bytes (used {} local \
                 objects)",
                stats.indexed_objects(),
                stats.total_objects(),
                stats.received_bytes(),
                stats.local_objects()
            );
        } else {
            println!(
                "\rReceived {}/{} objects in {} bytes",
                stats.indexed_objects(),
                stats.total_objects(),
                stats.received_bytes()
            );
        }
    }

    // Disconnect the underlying connection to prevent from idling.
    remote.disconnect()?;

    // Update the references in the remote's namespace to point to the right
    // commits. This may be needed even if there was no packfile to download,
    // which can happen e.g. when the branches have been changed but all the
    // needed objects are available locally.
    remote.update_tips(
        None,
        RemoteUpdateFlags::UPDATE_FETCHHEAD,
        AutotagOption::Unspecified,
        None,
    )?;

    let fetch_head = repo.find_reference("FETCH_HEAD")?;
    let fetch_commit = repo.reference_to_annotated_commit(&fetch_head)?;

    let refname = "refs/heads/master";
    match repo.find_reference(refname) {
        Ok(mut r) => {
            fast_forward(&repo, &mut r, &fetch_commit)?;
        }
        Err(_) => {
            // The branch doesn't exist so just set the reference to the
            // commit directly. Usually this is because you are pulling
            // into an empty repository.
            repo.reference(
                refname,
                fetch_commit.id(),
                true,
                &format!("Setting master to {}", fetch_commit.id()),
            )?;
            repo.set_head(refname)?;
            repo.checkout_head(Some(
                git2::build::CheckoutBuilder::default()
                    .allow_conflicts(true)
                    .conflict_style_merge(true)
                    .force(),
            ))?;
        }
    }

    Ok(())
}

fn fast_forward(
    repo: &Repository,
    lb: &mut git2::Reference,
    rc: &git2::AnnotatedCommit,
) -> Result<(), git2::Error> {
    let name = match lb.name() {
        Some(s) => s.to_string(),
        None => String::from_utf8_lossy(lb.name_bytes()).to_string(),
    };
    let msg = format!("Fast-Forward: Setting {} to id: {}", name, rc.id());
    println!("{}", msg);
    lb.set_target(rc.id(), &msg)?;
    repo.set_head(&name)?;
    repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))?;
    Ok(())
}

pub fn write_config_json(url: &str, mut writer: impl Write) -> std::io::Result<()> {
    let f = format!(
        r#"{{
  "dl": "{url}/crates/{{prefix}}/{{crate}}/{{version}}/{{crate}}-{{version}}.crate",
  "api": "{url}/crates"
}}
"#
    );
    writer.write_all(f.as_bytes())?;
    Ok(())
}
