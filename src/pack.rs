//! The zerus pack file: a single self-describing container that carries one transfer.
//!
//! Layout is a small header followed by a SquashFS image:
//!
//! ```text
//! magic "ZERUSPK\0" | u32 format version | SquashFS (zstd)
//! ```
//!
//! Inside the image:
//!
//! ```text
//! /manifest.txt                       name@version per line
//! /crates/{prefix}/{name}/{version}/  the .crate files, mirror layout
//! ```
//!
//! The manifest travels with the crates, so the receiving side does not need the sending
//! side's bookkeeping to know what arrived.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use backhand::compression::Compressor;
use backhand::{
    FilesystemCompressor, FilesystemReader, FilesystemWriter, InnerNode, NodeHeader,
    SquashfsFileReader,
};
use tracing::{debug, info};

use crate::index::find_crate_files;
use crate::{get_crate_path, Crate};

/// Identifies a zerus pack file. Checked before anything else is read.
const MAGIC: &[u8; 8] = b"ZERUSPK\0";

/// Bumped only for a change that an older zerus cannot read.
const FORMAT_VERSION: u32 = 1;

/// Bytes before the SquashFS image starts
const HEADER_LEN: u64 = MAGIC.len() as u64 + 4;

/// Path of the manifest inside the image
const MANIFEST_PATH: &str = "/manifest.txt";

/// `.crate` payloads are already gzip-compressed, so the block size matters more for the
/// index-shaped data than for the crates. 256 KiB keeps the block table small on a mirror
/// with thousands of files.
const BLOCK_SIZE: u32 = 256 * 1024;

/// Files in the image are data, never executed, so they carry plain read permissions.
fn node_header() -> NodeHeader {
    NodeHeader {
        permissions: 0o644,
        uid: 0,
        gid: 0,
        mtime: 0,
    }
}

/// What a pack write produced
#[derive(Debug)]
pub struct PackSummary {
    pub crates: Vec<Crate>,
    /// Size of the finished pack file
    pub bytes: u64,
}

/// Write `crates` from the mirror into a pack at `output`.
///
/// Every crate must have its `.crate` file present in the mirror.
pub fn write(mirror_path: &Path, crates: &[Crate], output: &Path) -> anyhow::Result<PackSummary> {
    if crates.is_empty() {
        bail!("no crates to pack");
    }

    let mut fs_writer = FilesystemWriter::default();
    fs_writer.set_block_size(BLOCK_SIZE);
    fs_writer.set_current_time();
    fs_writer.set_compressor(
        FilesystemCompressor::new(Compressor::Zstd, None)
            .context("failed to set up zstd compression")?,
    );

    let manifest = manifest_bytes(crates);
    fs_writer
        .push_file(std::io::Cursor::new(manifest), MANIFEST_PATH, node_header())
        .context("failed to add manifest to pack")?;

    // Pushed by path, so the writer opens each file only while it reads it. Handing it open
    // files would need one descriptor per crate, and a mirror runs to thousands.
    for c in crates {
        let path = crate_file_path(mirror_path, c)?;
        if !path.is_file() {
            bail!("failed to open {}", path.display());
        }

        let inner = inner_crate_path(c)?;
        let parent = inner
            .parent()
            .context("crate path inside pack has no parent")?;
        fs_writer
            .push_dir_all(parent, node_header())
            .with_context(|| format!("failed to create {} in pack", parent.display()))?;
        debug!("packing {}@{}", c.name, c.version);
        fs_writer
            .push_file_from_path(path, &inner, node_header())
            .with_context(|| format!("failed to add {} to pack", inner.display()))?;
    }

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
    }

    let file =
        File::create(output).with_context(|| format!("failed to create {}", output.display()))?;
    let mut out = BufWriter::new(file);
    out.write_all(MAGIC)?;
    out.write_all(&FORMAT_VERSION.to_le_bytes())?;
    fs_writer
        .write_with_offset(&mut out, HEADER_LEN)
        .context("failed to write squashfs image")?;
    out.flush()?;
    drop(out);

    let bytes = fs::metadata(output)
        .with_context(|| format!("failed to stat {}", output.display()))?
        .len();

    Ok(PackSummary {
        crates: crates.to_vec(),
        bytes,
    })
}

/// `name@version` lines, the same format the loose manifest files use
fn manifest_bytes(crates: &[Crate]) -> Vec<u8> {
    let mut buf = String::new();
    for c in crates {
        buf.push_str(&c.name);
        buf.push('@');
        buf.push_str(&c.version);
        buf.push('\n');
    }

    buf.into_bytes()
}

/// Where a crate's file lives in the mirror
fn crate_file_path(mirror_path: &Path, c: &Crate) -> anyhow::Result<PathBuf> {
    let dir = get_crate_path(mirror_path, &c.name, &c.version)
        .with_context(|| format!("invalid crate name: {}", c.name))?;

    Ok(dir.join(format!("{}-{}.crate", c.name, c.version)))
}

/// Where a crate's file lives inside the pack, mirroring the on-disk layout
fn inner_crate_path(c: &Crate) -> anyhow::Result<PathBuf> {
    let prefix = crate::get_index_prefix(&c.name)
        .with_context(|| format!("invalid crate name: {}", c.name))?;

    Ok(Path::new("/crates")
        .join(prefix)
        .join(&c.name)
        .join(&c.version)
        .join(format!("{}-{}.crate", c.name, c.version)))
}

/// Read the header and fail early on a file that is not a pack
fn read_header(file: &mut File, path: &Path) -> anyhow::Result<()> {
    let mut magic = [0u8; 8];
    file.read_exact(&mut magic)
        .with_context(|| format!("{} is too short to be a pack file", path.display()))?;
    if &magic != MAGIC {
        bail!("{} is not a zerus pack file", path.display());
    }

    let mut version = [0u8; 4];
    file.read_exact(&mut version)
        .with_context(|| format!("{} is truncated", path.display()))?;
    let version = u32::from_le_bytes(version);
    if version > FORMAT_VERSION {
        bail!(
            "{} uses pack format {version}, but this zerus reads up to {FORMAT_VERSION}; upgrade zerus",
            path.display()
        );
    }

    Ok(())
}

/// What a merge added to the mirror
#[derive(Debug)]
pub struct MergeSummary {
    /// Crates written into the mirror
    pub added: Vec<Crate>,
    /// Crates the mirror already had, left untouched
    pub skipped: Vec<Crate>,
    /// Everything the pack listed, added and skipped alike. This is what gets recorded as
    /// the transfer: the pack carried these crates, whether or not the mirror needed them.
    pub manifest: Vec<Crate>,
}

impl MergeSummary {
    /// Did the merge change the mirror? A pack of entirely known crates does not, so the
    /// caller can skip the index rebuild.
    pub fn changed(&self) -> bool {
        !self.added.is_empty()
    }
}

/// Read the manifest from a pack without unpacking the crates
pub fn read_manifest(pack_path: &Path) -> anyhow::Result<Vec<Crate>> {
    let mut file =
        File::open(pack_path).with_context(|| format!("failed to open {}", pack_path.display()))?;
    read_header(&mut file, pack_path)?;
    file.seek(SeekFrom::Start(0))?;

    let reader = BufReader::new(file);
    let fs_reader = FilesystemReader::from_reader_with_offset(reader, HEADER_LEN)
        .with_context(|| format!("failed to read squashfs image in {}", pack_path.display()))?;

    manifest_from(&fs_reader, pack_path)
}

/// Parse `/manifest.txt` out of an opened image
fn manifest_from(fs_reader: &FilesystemReader, pack_path: &Path) -> anyhow::Result<Vec<Crate>> {
    for node in fs_reader.files() {
        if node.fullpath != Path::new(MANIFEST_PATH) {
            continue;
        }
        let InnerNode::File(file) = &node.inner else {
            bail!("{} in {} is not a file", MANIFEST_PATH, pack_path.display());
        };

        let mut contents = String::new();
        fs_reader
            .file(file)
            .reader()
            .read_to_string(&mut contents)
            .with_context(|| format!("failed to read manifest from {}", pack_path.display()))?;

        return parse_manifest(&contents, pack_path);
    }

    bail!("no manifest found in {}", pack_path.display())
}

/// `name@version` lines into crates
fn parse_manifest(contents: &str, pack_path: &Path) -> anyhow::Result<Vec<Crate>> {
    let mut crates = Vec::new();
    for (n, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, version)) = line.split_once('@') else {
            bail!(
                "{}: manifest line {}: expected name@version, found {line:?}",
                pack_path.display(),
                n + 1
            );
        };
        let c = Crate::new(name.to_string(), version.to_string());
        validate_crate_id(&c)
            .with_context(|| format!("{}: manifest line {}", pack_path.display(), n + 1))?;
        // A repeated line stages the same file two times. Keep one entry.
        if !crates.contains(&c) {
            crates.push(c);
        }
    }

    Ok(crates)
}

/// Longest crate name crates.io accepts
const MAX_NAME_LEN: usize = 64;

/// Refuse an entry that is not a legal crate name and semver version.
///
/// Both become components of paths in the mirror, and a pack can come from anyone who has
/// the upload token. This check stops a name or version such as `../x` from writing outside
/// the mirror.
fn validate_crate_id(c: &Crate) -> anyhow::Result<()> {
    let name_is_legal = c.name.len() <= MAX_NAME_LEN
        && c.name.starts_with(|ch: char| ch.is_ascii_alphabetic())
        && c.name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
    if !name_is_legal {
        bail!("{:?} is not a legal crate name", c.name);
    }

    semver::Version::parse(&c.version)
        .with_context(|| format!("{:?} is not a legal semver version", c.version))?;

    Ok(())
}

/// Refuse a `.crate` whose own Cargo.toml does not name the crate the manifest says it is.
///
/// The index is built from the Cargo.toml inside each file, so without this check a pack
/// could list `foo@1.0.0` and put a new version of some other crate into the index. It also
/// runs the parse that `update_index` runs, so a file that passes cannot break the index.
fn verify_contents(path: &Path, c: &Crate) -> anyhow::Result<()> {
    let manifest = crate::index::extract_cargo_toml(path)
        .with_context(|| format!("{}@{} is not a valid .crate file", c.name, c.version))?;
    let package = &manifest.package;
    if package.name != c.name || package.version != c.version {
        bail!(
            "{}@{} holds a Cargo.toml for {}@{}",
            c.name,
            c.version,
            package.name,
            package.version
        );
    }

    Ok(())
}

/// Merge the crates in `pack_path` into the mirror at `mirror_path`.
///
/// A crate the mirror already holds is left alone, so re-applying a pack is safe.
/// The caller updates the index; this only moves crate files.
///
/// All or nothing: every new crate is extracted and checked before any of them goes into
/// the mirror, so one bad file in a pack leaves the mirror as it was.
pub fn merge(pack_path: &Path, mirror_path: &Path) -> anyhow::Result<MergeSummary> {
    let mut file =
        File::open(pack_path).with_context(|| format!("failed to open {}", pack_path.display()))?;
    read_header(&mut file, pack_path)?;
    file.seek(SeekFrom::Start(0))?;

    let reader = BufReader::new(file);
    let fs_reader = FilesystemReader::from_reader_with_offset(reader, HEADER_LEN)
        .with_context(|| format!("failed to read squashfs image in {}", pack_path.display()))?;

    let manifest = manifest_from(&fs_reader, pack_path)?;

    // Indexed once, so each lookup below does not walk the whole image again.
    let files: HashMap<&Path, &SquashfsFileReader> = fs_reader
        .files()
        .filter_map(|node| match &node.inner {
            InnerNode::File(file) => Some((node.fullpath.as_path(), file)),
            _ => None,
        })
        .collect();

    let mut staging = Staging::new(mirror_path)?;
    let mut skipped = Vec::new();
    for c in &manifest {
        let dest = crate_file_path(mirror_path, c)?;
        if dest.is_file() {
            debug!("{}@{} already in mirror", c.name, c.version);
            skipped.push(c.clone());
            continue;
        }

        let inner = inner_crate_path(c)?;
        let inner_file = files.get(inner.as_path()).with_context(|| {
            format!(
                "{} lists {}@{} but does not contain it",
                pack_path.display(),
                c.name,
                c.version
            )
        })?;
        staging.stage(&fs_reader, inner_file, c, dest)?;
    }

    let added = staging.commit()?;

    info!(
        "merged {} crate(s) into {} ({} already present)",
        added.len(),
        mirror_path.display(),
        skipped.len()
    );

    Ok(MergeSummary {
        added,
        skipped,
        manifest,
    })
}

/// A new crate, extracted and checked, waiting to go into the mirror
struct StagedCrate {
    krate: Crate,
    staged: PathBuf,
    dest: PathBuf,
}

/// The crates of one merge, held back from the mirror until every one of them passes.
///
/// They wait in a temporary directory inside the mirror, so the final rename does not cross
/// filesystems, and the index never sees them because it only scans `crates/`. A merge that
/// fails, by error or by panic, drops the directory and leaves the mirror as it was.
struct Staging {
    dir: tempfile::TempDir,
    crates: Vec<StagedCrate>,
}

impl Staging {
    fn new(mirror_path: &Path) -> anyhow::Result<Self> {
        fs::create_dir_all(mirror_path)
            .with_context(|| format!("failed to create {}", mirror_path.display()))?;
        let dir = tempfile::Builder::new()
            .prefix(".merge-")
            .tempdir_in(mirror_path)
            .with_context(|| {
                format!(
                    "failed to create a staging directory in {}",
                    mirror_path.display()
                )
            })?;

        Ok(Self {
            dir,
            crates: Vec::new(),
        })
    }

    /// Extract `file` and check that it is the crate `c` claims to be
    fn stage(
        &mut self,
        fs_reader: &FilesystemReader,
        file: &SquashfsFileReader,
        c: &Crate,
        dest: PathBuf,
    ) -> anyhow::Result<()> {
        // The manifest holds each crate once, so the name is unique in the directory.
        let staged = self
            .dir
            .path()
            .join(format!("{}-{}.crate", c.name, c.version));
        let mut out = BufWriter::new(
            File::create(&staged)
                .with_context(|| format!("failed to create {}", staged.display()))?,
        );
        let mut src = fs_reader.file(file).reader();
        std::io::copy(&mut src, &mut out)
            .with_context(|| format!("failed to extract {}@{}", c.name, c.version))?;
        out.flush()?;
        drop(out);

        verify_contents(&staged, c)?;
        self.crates.push(StagedCrate {
            krate: c.clone(),
            staged,
            dest,
        });

        Ok(())
    }

    /// Move every staged crate into the mirror and return them
    fn commit(self) -> anyhow::Result<Vec<Crate>> {
        self.crates
            .into_iter()
            .map(|s| {
                let parent = s.dest.parent().context("crate path has no parent")?;
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
                fs::rename(&s.staged, &s.dest)
                    .with_context(|| format!("failed to write {}", s.dest.display()))?;
                debug!("added {}@{}", s.krate.name, s.krate.version);
                Ok(s.krate)
            })
            .collect()
    }
}

/// Name the manifest that records the transfer a pack carried.
///
/// Derived from the pack file name, so a record traces back to the file that carried it.
/// `file_stem` drops any directory part, which also keeps an uploaded pack from naming a
/// path outside the manifests directory.
pub fn manifest_name(pack_name: &Path) -> String {
    let stem = pack_name
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("transfer"));

    format!("manifest-{stem}.txt")
}

/// Record a merged pack in the manifests directory, so the receiving side has the same
/// record of the transfer that the sending side kept.
///
/// The file is named after the pack. Re-applying a pack finds its own record and adds no
/// second one.
pub fn record_merge(
    manifests_dir: &Path,
    pack_name: &str,
    summary: &MergeSummary,
) -> anyhow::Result<PathBuf> {
    record(
        manifests_dir,
        &manifest_name(Path::new(pack_name)),
        &summary.manifest,
    )
}

/// All crates in the mirror, minus those any manifest already records.
///
/// This is the `generate-manifest` plus `cull` pair as one step: instead of deleting the
/// already-transferred crates from the mirror, it selects the ones that still have to go.
pub fn pending(mirror_path: &Path, manifests_dir: Option<&Path>) -> anyhow::Result<Vec<Crate>> {
    // A project with no crates.io dependencies downloads nothing, so there is no crates/.
    if !mirror_path.join("crates").is_dir() {
        return Ok(Vec::new());
    }

    let manifests = match manifests_dir {
        Some(dir) if dir.is_dir() => crate::manifest::load_dir(dir)?,
        // A first transfer has no manifests dir yet; everything in the mirror is pending.
        _ => Vec::new(),
    };

    crate::manifest::unmanifested(mirror_path, &manifests)
}

/// Most records `record` tries before it gives up on finding a free name
const MAX_RECORD_NAMES: u32 = 10_000;

/// Record `crates` as transferred by writing a manifest file into `dir`.
///
/// Pack names can repeat. For example, a sender who always writes
/// `-o /media/usb/transfer.zpk` gives each record the name `manifest-transfer.txt`. This
/// function does not replace a different record, because then its crates go again in the
/// next pack. The new record gets the next free name: `manifest-transfer-2.txt`, then `-3`,
/// and so on. A record with the same crates is the same transfer applied again, so this
/// function uses that record and writes nothing.
pub fn record(dir: &Path, name: &str, crates: &[Crate]) -> anyhow::Result<PathBuf> {
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let contents = manifest_bytes(crates);

    let name = Path::new(name);
    let stem = name
        .file_stem()
        .unwrap_or(name.as_os_str())
        .to_string_lossy();
    let extension = name
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let candidates = core::iter::once(dir.join(name))
        .chain((2..=MAX_RECORD_NAMES).map(|n| dir.join(format!("{stem}-{n}{extension}"))));

    for path in candidates {
        // `create_new` gets the name in one atomic step, so two records that are written at
        // the same time cannot both get it.
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(&contents)
                    .with_context(|| format!("failed to write {}", path.display()))?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = fs::read(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                if existing == contents {
                    return Ok(path);
                }
            }
            Err(e) => {
                return Err(e).with_context(|| format!("failed to create {}", path.display()))
            }
        }
    }

    bail!(
        "{} already holds {MAX_RECORD_NAMES} records named like {}",
        dir.display(),
        name.display()
    )
}

/// Count the `.crate` files in a mirror, for reporting
pub fn count_crates(mirror_path: &Path) -> usize {
    find_crate_files(&mirror_path.join("crates")).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `.crate`: a gzipped tar holding `{name}-{version}/Cargo.toml`, plus a `marker`
    /// file so that two crates of one name and version can differ byte for byte.
    fn crate_bytes(name: &str, version: &str, marker: &[u8]) -> Vec<u8> {
        let cargo_toml = format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\n");
        let mut tar = tar::Builder::new(Vec::new());
        for (file, data) in [("Cargo.toml", cargo_toml.as_bytes()), ("marker", marker)] {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, format!("{name}-{version}/{file}"), data)
                .unwrap();
        }

        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&tar.into_inner().unwrap()).unwrap();
        gz.finish().unwrap()
    }

    /// Put `bytes` in the mirror where the `.crate` for `name@version` goes
    fn add_raw(mirror: &Path, name: &str, version: &str, bytes: &[u8]) {
        let dir = get_crate_path(mirror, name, version).unwrap();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{name}-{version}.crate")), bytes).unwrap();
    }

    /// Put a valid `.crate` file in the mirror at the layout the pack expects
    fn add_crate(mirror: &Path, name: &str, version: &str, marker: &[u8]) {
        add_raw(mirror, name, version, &crate_bytes(name, version, marker));
    }

    fn krate(name: &str, version: &str) -> Crate {
        Crate::new(name.to_string(), version.to_string())
    }

    fn names(crates: &[Crate]) -> Vec<String> {
        crates
            .iter()
            .map(|c| format!("{}@{}", c.name, c.version))
            .collect()
    }

    #[test]
    fn round_trip_restores_every_crate_byte_for_byte() {
        let src = tempfile::tempdir().unwrap();
        add_crate(src.path(), "serde", "1.0.210", b"serde-payload");
        add_crate(src.path(), "a", "0.1.0", b"single-letter-name");
        add_crate(src.path(), "tokio", "1.40.0", b"tokio-payload");

        let pack = src.path().join("out.zpk");
        let crates = [
            krate("serde", "1.0.210"),
            krate("a", "0.1.0"),
            krate("tokio", "1.40.0"),
        ];
        let summary = write(src.path(), &crates, &pack).unwrap();
        assert_eq!(summary.crates.len(), 3);
        assert!(summary.bytes > 0);

        let dest = tempfile::tempdir().unwrap();
        let merged = merge(&pack, dest.path()).unwrap();

        assert_eq!(merged.added.len(), 3);
        assert!(merged.skipped.is_empty());
        assert!(merged.changed());
        for (name, version, contents) in [
            ("serde", "1.0.210", b"serde-payload".as_slice()),
            ("a", "0.1.0", b"single-letter-name".as_slice()),
            ("tokio", "1.40.0", b"tokio-payload".as_slice()),
        ] {
            let path = crate_file_path(dest.path(), &krate(name, version)).unwrap();
            assert_eq!(
                fs::read(&path).unwrap(),
                crate_bytes(name, version, contents),
                "{name}@{version}"
            );
        }
    }

    /// A mirror holds thousands of crates. Opening them all before writing exhausts the
    /// process descriptor limit, so the writer must hold at most a few open at a time.
    #[test]
    fn packing_more_crates_than_the_descriptor_limit_succeeds() {
        let src = tempfile::tempdir().unwrap();

        // Comfortably over the usual 1024 soft limit.
        let count = 2000;
        let crates: Vec<Crate> = (0..count)
            .map(|n| {
                let name = format!("crate{n:05}");
                add_crate(src.path(), &name, "1.0.0", name.as_bytes());
                krate(&name, "1.0.0")
            })
            .collect();

        let pack = src.path().join("many.zpk");
        let summary = write(src.path(), &crates, &pack).unwrap();
        assert_eq!(summary.crates.len(), count);

        // Reading them back must stay within the limit too.
        let dest = tempfile::tempdir().unwrap();
        let merged = merge(&pack, dest.path()).unwrap();
        assert_eq!(merged.added.len(), count);

        // Spot-check that content still matches its own crate, not a neighbour's.
        for n in [0, count / 2, count - 1] {
            let name = format!("crate{n:05}");
            let path = crate_file_path(dest.path(), &krate(&name, "1.0.0")).unwrap();
            assert_eq!(
                fs::read(&path).unwrap(),
                crate_bytes(&name, "1.0.0", name.as_bytes())
            );
        }
    }

    #[test]
    fn a_merge_summary_carries_the_whole_manifest_not_just_the_new_crates() {
        let src = tempfile::tempdir().unwrap();
        add_crate(src.path(), "serde", "1.0.210", b"a");
        add_crate(src.path(), "tokio", "1.40.0", b"b");
        let pack = src.path().join("out.zpk");
        write(
            src.path(),
            &[krate("serde", "1.0.210"), krate("tokio", "1.40.0")],
            &pack,
        )
        .unwrap();

        // The mirror already has one of them, so it is skipped rather than added.
        let dest = tempfile::tempdir().unwrap();
        add_crate(dest.path(), "serde", "1.0.210", b"a");
        let summary = merge(&pack, dest.path()).unwrap();

        assert_eq!(names(&summary.added), ["tokio@1.40.0"]);
        // The record is of what the pack carried, not of what the mirror happened to need.
        assert_eq!(names(&summary.manifest), ["serde@1.0.210", "tokio@1.40.0"]);
    }

    #[test]
    fn a_manifest_is_named_for_the_pack_that_carried_it() {
        assert_eq!(
            manifest_name(Path::new("2026-09-06.zpk")),
            "manifest-2026-09-06.txt"
        );
        // The directory part is dropped, so only the file name decides the record.
        assert_eq!(
            manifest_name(Path::new("transfers/2026-09-06.zpk")),
            "manifest-2026-09-06.txt"
        );
        // A name with nothing to take a stem from still produces a usable file.
        assert_eq!(manifest_name(Path::new("..")), "manifest-transfer.txt");
    }

    #[test]
    fn record_merge_writes_the_packs_manifest_next_to_the_mirror() {
        let src = tempfile::tempdir().unwrap();
        add_crate(src.path(), "serde", "1.0.210", b"a");
        let pack = src.path().join("2026-09-06.zpk");
        write(src.path(), &[krate("serde", "1.0.210")], &pack).unwrap();

        let dest = tempfile::tempdir().unwrap();
        let summary = merge(&pack, dest.path()).unwrap();
        let dir = dest.path().join("manifests");

        let path = record_merge(&dir, "2026-09-06.zpk", &summary).unwrap();

        // Named after the pack, and readable by the same loader the web UI uses.
        assert_eq!(path.file_name().unwrap(), "manifest-2026-09-06.txt");
        let loaded = crate::manifest::load_dir(&dir).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(names(&loaded[0].crates), ["serde@1.0.210"]);

        // With the transfer recorded, nothing in the mirror is un-manifested.
        assert!(crate::manifest::unmanifested(dest.path(), &loaded)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn record_merge_keeps_an_upload_name_inside_the_manifests_directory() {
        let src = tempfile::tempdir().unwrap();
        add_crate(src.path(), "serde", "1.0.210", b"a");
        let pack = src.path().join("out.zpk");
        write(src.path(), &[krate("serde", "1.0.210")], &pack).unwrap();
        let dest = tempfile::tempdir().unwrap();
        let summary = merge(&pack, dest.path()).unwrap();
        let dir = dest.path().join("manifests");

        // An uploaded pack names itself, so the name is not trusted as a path.
        let path = record_merge(&dir, "../../etc/passwd.zpk", &summary).unwrap();

        assert_eq!(path, dir.join("manifest-passwd.txt"));
        assert!(
            path.starts_with(&dir),
            "escaped the manifests dir: {path:?}"
        );
    }

    #[test]
    fn re_recording_a_pack_rewrites_one_record_rather_than_adding_another() {
        let src = tempfile::tempdir().unwrap();
        add_crate(src.path(), "serde", "1.0.210", b"a");
        let pack = src.path().join("2026-09-06.zpk");
        write(src.path(), &[krate("serde", "1.0.210")], &pack).unwrap();
        let dest = tempfile::tempdir().unwrap();
        let dir = dest.path().join("manifests");

        for _ in 0..2 {
            let summary = merge(&pack, dest.path()).unwrap();
            record_merge(&dir, "2026-09-06.zpk", &summary).unwrap();
        }

        let loaded = crate::manifest::load_dir(&dir).unwrap();
        assert_eq!(loaded.len(), 1, "a re-applied pack made a second record");
        assert_eq!(names(&loaded[0].crates), ["serde@1.0.210"]);
    }

    #[test]
    fn merge_is_idempotent() {
        let src = tempfile::tempdir().unwrap();
        add_crate(src.path(), "serde", "1.0.210", b"payload");
        let pack = src.path().join("out.zpk");
        write(src.path(), &[krate("serde", "1.0.210")], &pack).unwrap();

        let dest = tempfile::tempdir().unwrap();
        merge(&pack, dest.path()).unwrap();
        let second = merge(&pack, dest.path()).unwrap();

        assert!(second.added.is_empty());
        assert_eq!(names(&second.skipped), ["serde@1.0.210"]);
        assert!(!second.changed(), "no change means no index rebuild");
    }

    #[test]
    fn merge_keeps_a_crate_the_mirror_already_has() {
        let src = tempfile::tempdir().unwrap();
        add_crate(src.path(), "serde", "1.0.210", b"from-pack");
        let pack = src.path().join("out.zpk");
        write(src.path(), &[krate("serde", "1.0.210")], &pack).unwrap();

        let dest = tempfile::tempdir().unwrap();
        add_crate(dest.path(), "serde", "1.0.210", b"already-here");
        merge(&pack, dest.path()).unwrap();

        let path = crate_file_path(dest.path(), &krate("serde", "1.0.210")).unwrap();
        assert_eq!(
            fs::read(&path).unwrap(),
            crate_bytes("serde", "1.0.210", b"already-here")
        );
    }

    #[test]
    fn merge_adds_only_the_new_crates_to_a_populated_mirror() {
        let src = tempfile::tempdir().unwrap();
        add_crate(src.path(), "serde", "1.0.210", b"a");
        add_crate(src.path(), "tokio", "1.40.0", b"b");
        let pack = src.path().join("out.zpk");
        write(
            src.path(),
            &[krate("serde", "1.0.210"), krate("tokio", "1.40.0")],
            &pack,
        )
        .unwrap();

        let dest = tempfile::tempdir().unwrap();
        add_crate(dest.path(), "serde", "1.0.210", b"a");
        let merged = merge(&pack, dest.path()).unwrap();

        assert_eq!(names(&merged.added), ["tokio@1.40.0"]);
        assert_eq!(names(&merged.skipped), ["serde@1.0.210"]);
    }

    #[test]
    fn read_manifest_lists_the_packed_crates_without_unpacking() {
        let src = tempfile::tempdir().unwrap();
        add_crate(src.path(), "serde", "1.0.210", b"a");
        add_crate(src.path(), "tokio", "1.40.0", b"b");
        let pack = src.path().join("out.zpk");
        write(
            src.path(),
            &[krate("serde", "1.0.210"), krate("tokio", "1.40.0")],
            &pack,
        )
        .unwrap();

        let manifest = read_manifest(&pack).unwrap();

        assert_eq!(names(&manifest), ["serde@1.0.210", "tokio@1.40.0"]);
    }

    #[test]
    fn a_file_that_is_not_a_pack_is_rejected_by_magic() {
        let tmp = tempfile::tempdir().unwrap();
        let bogus = tmp.path().join("not-a-pack.zpk");
        fs::write(&bogus, b"this is definitely not a squashfs image").unwrap();

        let err = merge(&bogus, tmp.path()).unwrap_err().to_string();

        assert!(
            err.contains("not a zerus pack file"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn a_truncated_file_is_rejected_before_the_image_is_read() {
        let tmp = tempfile::tempdir().unwrap();
        let short = tmp.path().join("short.zpk");
        fs::write(&short, b"ZE").unwrap();

        assert!(merge(&short, tmp.path()).is_err());
    }

    #[test]
    fn a_newer_format_version_is_reported_as_needing_an_upgrade() {
        let tmp = tempfile::tempdir().unwrap();
        let future = tmp.path().join("future.zpk");
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&(FORMAT_VERSION + 1).to_le_bytes());
        bytes.extend_from_slice(b"whatever comes next");
        fs::write(&future, bytes).unwrap();

        let err = merge(&future, tmp.path()).unwrap_err().to_string();

        assert!(err.contains("upgrade zerus"), "unexpected error: {err}");
    }

    #[test]
    fn packing_nothing_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(write(tmp.path(), &[], &tmp.path().join("out.zpk")).is_err());
    }

    #[test]
    fn packing_a_crate_missing_from_the_mirror_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = write(
            tmp.path(),
            &[krate("absent", "1.0.0")],
            &tmp.path().join("out.zpk"),
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("failed to open"), "unexpected error: {err}");
    }

    #[test]
    fn pending_skips_what_the_manifests_already_record() {
        let mirror = tempfile::tempdir().unwrap();
        add_crate(mirror.path(), "serde", "1.0.210", b"a");
        add_crate(mirror.path(), "tokio", "1.40.0", b"b");

        let manifests = tempfile::tempdir().unwrap();
        fs::write(manifests.path().join("2026-01-01.txt"), "serde@1.0.210\n").unwrap();

        let crates = pending(mirror.path(), Some(manifests.path())).unwrap();

        assert_eq!(names(&crates), ["tokio@1.40.0"]);
    }

    #[test]
    fn pending_on_a_first_transfer_is_the_whole_mirror() {
        let mirror = tempfile::tempdir().unwrap();
        add_crate(mirror.path(), "serde", "1.0.210", b"a");

        // No manifests dir exists yet on the first run.
        let crates = pending(mirror.path(), Some(&mirror.path().join("manifests"))).unwrap();

        assert_eq!(names(&crates), ["serde@1.0.210"]);
    }

    #[test]
    fn pending_on_a_mirror_with_nothing_downloaded_is_empty() {
        let mirror = tempfile::tempdir().unwrap();

        let crates = pending(mirror.path(), None).unwrap();

        assert!(crates.is_empty());
    }

    #[test]
    fn a_second_transfer_under_the_same_pack_name_keeps_the_first_record() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("manifests");

        let first = record(&dir, "manifest-transfer.txt", &[krate("itoa", "1.0.18")]).unwrap();
        let second = record(&dir, "manifest-transfer.txt", &[krate("hex", "0.4.3")]).unwrap();
        let third = record(&dir, "manifest-transfer.txt", &[krate("cfg-if", "1.0.5")]).unwrap();

        assert_eq!(first, dir.join("manifest-transfer.txt"));
        assert_eq!(second, dir.join("manifest-transfer-2.txt"));
        assert_eq!(third, dir.join("manifest-transfer-3.txt"));
        assert_eq!(fs::read_to_string(&first).unwrap(), "itoa@1.0.18\n");
        assert_eq!(fs::read_to_string(&second).unwrap(), "hex@0.4.3\n");
    }

    #[test]
    fn recording_the_same_transfer_again_reuses_its_record() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("manifests");
        record(&dir, "manifest-a.txt", &[krate("itoa", "1.0.18")]).unwrap();
        let second = record(&dir, "manifest-a.txt", &[krate("hex", "0.4.3")]).unwrap();

        // Applied again after a different transfer took the -2 name: still no new record.
        let again = record(&dir, "manifest-a.txt", &[krate("hex", "0.4.3")]).unwrap();

        assert_eq!(again, second);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
    }

    #[test]
    fn records_written_at_once_under_one_name_all_survive() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("manifests");

        let paths: Vec<PathBuf> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..16)
                .map(|n| {
                    let dir = &dir;
                    scope.spawn(move || {
                        record(
                            dir,
                            "manifest-x.txt",
                            &[krate(&format!("c{n:03}"), "1.0.0")],
                        )
                        .unwrap()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        let loaded = crate::manifest::load_dir(&dir).unwrap();
        assert_eq!(loaded.len(), 16, "records were lost: {paths:?}");
    }

    #[test]
    fn record_writes_a_manifest_the_loose_format_can_read() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("manifests");

        let path = record(&dir, "2026-09-06.txt", &[krate("serde", "1.0.210")]).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "serde@1.0.210\n");
        let loaded = crate::manifest::load_dir(&dir).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(names(&loaded[0].crates), ["serde@1.0.210"]);
    }

    #[test]
    fn a_manifest_line_without_a_version_is_rejected() {
        let err = parse_manifest("serde\n", Path::new("p.zpk"))
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("expected name@version"),
            "unexpected error: {err}"
        );
    }

    /// Write a pack by hand, with any manifest and any paths, as an attacker can
    fn raw_pack(out: &Path, manifest: &str, files: &[(&str, &[u8])]) {
        let mut fs_writer = FilesystemWriter::default();
        fs_writer.set_compressor(FilesystemCompressor::new(Compressor::Zstd, None).unwrap());
        fs_writer
            .push_file(
                std::io::Cursor::new(manifest.as_bytes().to_vec()),
                MANIFEST_PATH,
                node_header(),
            )
            .unwrap();
        for (path, data) in files {
            let parent = Path::new(path).parent().unwrap();
            fs_writer.push_dir_all(parent, node_header()).unwrap();
            fs_writer
                .push_file(std::io::Cursor::new(data.to_vec()), *path, node_header())
                .unwrap();
        }

        let mut file = BufWriter::new(File::create(out).unwrap());
        file.write_all(MAGIC).unwrap();
        file.write_all(&FORMAT_VERSION.to_le_bytes()).unwrap();
        fs_writer.write_with_offset(&mut file, HEADER_LEN).unwrap();
    }

    /// Every file under `dir`, for checking that a failed merge left nothing behind
    fn all_files(dir: &Path) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .flat_map(|e| {
                let path = e.path();
                if path.is_dir() {
                    all_files(&path)
                } else {
                    vec![path]
                }
            })
            .collect()
    }

    #[test]
    fn a_manifest_entry_that_climbs_out_of_the_mirror_is_rejected() {
        for line in [
            "serde@1.0.0/../../../../escape",
            "../../escape@1.0.0",
            "se/rde@1.0.0",
            "serde@..",
        ] {
            let err = format!(
                "{:#}",
                parse_manifest(line, Path::new("p.zpk")).unwrap_err()
            );
            assert!(
                err.contains("not a legal"),
                "{line}: unexpected error: {err}"
            );
        }
    }

    #[test]
    fn a_pack_that_names_a_path_outside_the_mirror_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let version = "1.0.0/../../../../../../escape";
        let inner = format!("/crates/se/rd/serde/{version}/serde-{version}.crate");
        let pack = tmp.path().join("hostile.zpk");
        raw_pack(
            &pack,
            &format!("serde@{version}\n"),
            &[(&inner, b"payload")],
        );
        let mirror = tmp.path().join("deep/down/mirror");

        assert!(merge(&pack, &mirror).is_err());
        assert_eq!(all_files(tmp.path()), [pack]);
    }

    #[test]
    fn a_crate_that_is_not_a_gzipped_tar_is_rejected_and_leaves_no_trace() {
        let src = tempfile::tempdir().unwrap();
        add_raw(src.path(), "evil", "1.0.0", b"not a crate");
        let pack = src.path().join("out.zpk");
        write(src.path(), &[krate("evil", "1.0.0")], &pack).unwrap();

        let dest = tempfile::tempdir().unwrap();
        let err = format!("{:#}", merge(&pack, dest.path()).unwrap_err());

        assert!(
            err.contains("not a valid .crate file"),
            "unexpected error: {err}"
        );
        assert!(
            all_files(dest.path()).is_empty(),
            "left {:?}",
            all_files(dest.path())
        );
    }

    #[test]
    fn a_crate_whose_cargo_toml_names_another_crate_is_rejected() {
        let src = tempfile::tempdir().unwrap();
        // Listed as fake@1.0.0, but its Cargo.toml would index it as serde@9.9.9.
        add_raw(
            src.path(),
            "fake",
            "1.0.0",
            &crate_bytes("serde", "9.9.9", b"impostor"),
        );
        let pack = src.path().join("out.zpk");
        write(src.path(), &[krate("fake", "1.0.0")], &pack).unwrap();

        let dest = tempfile::tempdir().unwrap();
        let err = format!("{:#}", merge(&pack, dest.path()).unwrap_err());

        assert!(
            err.contains("holds a Cargo.toml for serde@9.9.9"),
            "unexpected error: {err}"
        );
        assert!(all_files(dest.path()).is_empty());
    }

    #[test]
    fn one_bad_crate_keeps_every_crate_in_the_pack_out_of_the_mirror() {
        let src = tempfile::tempdir().unwrap();
        add_crate(src.path(), "good", "1.0.0", b"fine");
        add_raw(src.path(), "zbad", "1.0.0", b"not a crate");
        let pack = src.path().join("out.zpk");
        write(
            src.path(),
            &[krate("good", "1.0.0"), krate("zbad", "1.0.0")],
            &pack,
        )
        .unwrap();

        let dest = tempfile::tempdir().unwrap();
        assert!(merge(&pack, dest.path()).is_err());

        assert!(
            all_files(dest.path()).is_empty(),
            "left {:?}",
            all_files(dest.path())
        );
    }

    #[test]
    fn a_repeated_manifest_line_is_merged_once() {
        let src = tempfile::tempdir().unwrap();
        let pack = src.path().join("out.zpk");
        let contents = crate_bytes("serde", "1.0.210", b"a");
        raw_pack(
            &pack,
            "serde@1.0.210\nserde@1.0.210\n",
            &[("/crates/se/rd/serde/1.0.210/serde-1.0.210.crate", &contents)],
        );

        let dest = tempfile::tempdir().unwrap();
        let summary = merge(&pack, dest.path()).unwrap();

        assert_eq!(names(&summary.added), ["serde@1.0.210"]);
        assert_eq!(names(&summary.manifest), ["serde@1.0.210"]);
    }

    proptest::proptest! {
        #[test]
        fn a_legal_crate_id_is_accepted(
            name in "[a-zA-Z][a-zA-Z0-9_-]{0,63}",
            (major, minor, patch) in (0u64..1000, 0u64..1000, 0u64..1000),
        ) {
            let c = krate(&name, &format!("{major}.{minor}.{patch}"));
            proptest::prop_assert!(validate_crate_id(&c).is_ok());
        }

        #[test]
        fn a_name_with_any_path_character_is_rejected(
            head in "[a-z]{1,10}",
            bad in r"[./\\ ]",
            tail in "[a-z]{0,10}",
        ) {
            let c = krate(&format!("{head}{bad}{tail}"), "1.0.0");
            proptest::prop_assert!(validate_crate_id(&c).is_err());
        }

        #[test]
        fn a_version_with_a_path_separator_is_rejected(
            prefix in r"[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}",
            tail in "[a-z.]{0,10}",
        ) {
            let c = krate("serde", &format!("{prefix}/{tail}"));
            proptest::prop_assert!(validate_crate_id(&c).is_err());
        }

        /// For each manifest, the parse fails, or it gives entries that stay in the mirror.
        #[test]
        fn a_parsed_manifest_never_names_a_path_outside_the_mirror(line in r"\PC{0,80}") {
            let mirror = Path::new("/mirror");
            if let Ok(crates) = parse_manifest(&line, Path::new("p.zpk")) {
                for c in crates {
                    let path = crate_file_path(mirror, &c).unwrap();
                    proptest::prop_assert!(
                        path.components().all(|part| matches!(
                            part,
                            std::path::Component::RootDir | std::path::Component::Normal(_)
                        )),
                        "{path:?}"
                    );
                    proptest::prop_assert!(path.starts_with("/mirror/crates"));
                }
            }
        }

        #[test]
        fn a_name_over_the_length_limit_is_rejected(name in "[a-z]{65,100}") {
            proptest::prop_assert!(validate_crate_id(&krate(&name, "1.0.0")).is_err());
        }
    }

    #[test]
    fn manifest_comments_and_blank_lines_are_skipped() {
        let crates =
            parse_manifest("# a comment\n\nserde@1.0.210\n  \n", Path::new("p.zpk")).unwrap();

        assert_eq!(names(&crates), ["serde@1.0.210"]);
    }
}
