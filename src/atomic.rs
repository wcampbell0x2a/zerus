//! Write files so that a reader never sees them half-written.
//!
//! `zerus serve` can run while `mirror` or `update-index` writes to the same mirror. A file
//! written in place is empty for a moment, and then partly written: a client that reads it
//! then gets a truncated `.crate` or index file. A crash in the middle of a write leaves such
//! a file behind for good.

use std::io::Write;
use std::path::Path;

/// Write `contents` to `path`, and replace the file that is there.
///
/// The data goes to a temporary file in the same directory first, which then takes the place
/// of `path` with one rename. A rename in one directory is atomic, so a reader sees either the
/// old file or the new file, never a part of one.
pub fn write(path: &Path, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    // A dotfile, so that a scan of the mirror, the index, or the manifests skips it
    let mut builder = tempfile::Builder::new();
    builder.prefix(".zerus-");
    // A temporary file is only for its owner by default. Give it the mode of a file from
    // `fs::write`, so that a server that runs as another user can still read it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    let mut file = builder.tempfile_in(dir)?;
    file.write_all(contents.as_ref())?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn write_creates_and_replaces_a_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.txt");

        write(&path, "first").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "first");
        write(&path, "second, which is longer").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "second, which is longer"
        );
        write(&path, "").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
    }

    #[test]
    fn write_leaves_no_temporary_file() {
        let tmp = tempfile::tempdir().unwrap();

        write(&tmp.path().join("a.txt"), "data").unwrap();

        let names: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["a.txt"]);
    }

    #[test]
    fn write_into_a_missing_dir_fails_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(write(&tmp.path().join("nope/a.txt"), "data").is_err());
        assert!(!tmp.path().join("nope").exists());
    }

    #[cfg(unix)]
    #[test]
    fn write_gives_the_same_mode_as_fs_write() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let ours = tmp.path().join("ours");
        let std = tmp.path().join("std");

        write(&ours, "data").unwrap();
        fs::write(&std, "data").unwrap();

        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&ours), mode(&std));
    }

    /// Model check: while one thread replaces a file again and again, a reader only ever sees
    /// one of the whole contents, never an empty or a partial file
    #[test]
    fn a_reader_never_sees_a_partial_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("index-file");
        let versions: Vec<String> = (0..4).map(|n| format!("{n}").repeat(64 * 1024)).collect();
        write(&path, &versions[0]).unwrap();

        std::thread::scope(|scope| {
            scope.spawn(|| {
                for n in 0..200 {
                    write(&path, &versions[n % versions.len()]).unwrap();
                }
            });
            for _ in 0..2000 {
                let seen = fs::read_to_string(&path).unwrap();
                assert!(versions.contains(&seen), "read {} bytes", seen.len());
            }
        });
    }
}
