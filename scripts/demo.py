#!/usr/bin/env python3
"""Make a demo mirror and manifests directory for `zerus serve`.

The demo copies the history of a real mirror: five transfers over five months. For each
transfer, crates are downloaded in the days before it, a manifest records them, and the
next cycle culls them. Some culled crates are downloaded again by a later `zerus mirror`,
and the crates downloaded after the last transfer are in no manifest.

The crate names and versions come from the local cargo cache, so they are real. If the
cache is empty, a built-in list of popular crates is used. The `.crate` files are empty,
as the manifest pages only read their names and write times.

Usage: demo.py <output dir>
"""

import datetime as dt
import os
import random
import re
import shutil
import sys
from pathlib import Path

CARGO_CACHE = Path.home() / ".cargo" / "registry" / "cache"
CRATE_FILE = re.compile(r"^(.+?)-(\d+\.\d+\.\d+.*)\.crate$")

# Used when the cargo cache has no crates
POPULAR = [
    "anyhow", "axum", "bitflags", "bytes", "cc", "cfg-if", "clap", "crossbeam", "either",
    "futures", "getrandom", "hashbrown", "http", "hyper", "indexmap", "itoa", "libc", "log",
    "memchr", "mio", "once_cell", "parking_lot", "proc-macro2", "quote", "rand", "rayon",
    "regex", "reqwest", "rustls", "ryu", "serde", "serde_json", "smallvec", "socket2", "syn",
    "thiserror", "time", "tokio", "tower", "tracing", "unicode-ident", "url", "uuid", "zerocopy",
]

# Days before now of each transfer, oldest first
TRANSFER_DAYS_AGO = [140, 98, 68, 30, 10]

# Chance that a crate culled after a transfer is in the mirror again
DOWNLOADED_AGAIN = 0.04


def crates_in_cache():
    """Real crate versions from the local cargo cache, as {name: [version]}"""
    by_name = {}
    if CARGO_CACHE.is_dir():
        for registry in CARGO_CACHE.iterdir():
            for file in registry.glob("*.crate"):
                match = CRATE_FILE.match(file.name)
                if match:
                    by_name.setdefault(match[1], set()).add(match[2])
    return {name: sorted(versions) for name, versions in by_name.items()}


def crates_built_in(rng):
    """A few versions of each popular crate, as {name: [version]}"""
    by_name = {}
    for name in POPULAR:
        major, minor = rng.randint(0, 1), rng.randint(1, 40)
        by_name[name] = [f"{major}.{minor + n}.{rng.randint(0, 9)}" for n in range(rng.randint(2, 5))]
    return by_name


def version_key(version):
    """Sort `1.10.0` after `1.9.0`"""
    return [int(part) if part.isdigit() else 0 for part in re.split(r"[.+-]", version)]


def crate_path(root, name, version):
    """The path that `zerus mirror` gives a `.crate` file"""
    if len(name) <= 2:
        prefix = str(len(name))
    elif len(name) == 3:
        prefix = f"3/{name[0]}"
    else:
        prefix = f"{name[:2]}/{name[2:4]}"
    return root / "crates" / prefix / name / version / f"{name}-{version}.crate"


def write_crate(mirror, name, version, when):
    path = crate_path(mirror, name, version)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.touch()
    os.utime(path, (when.timestamp(), when.timestamp()))


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    out = Path(sys.argv[1])
    mirror, manifests = out / "mirror", out / "manifests"
    shutil.rmtree(out, ignore_errors=True)
    manifests.mkdir(parents=True)

    # A fixed seed gives the same demo each time
    rng = random.Random(42)
    by_name = crates_in_cache() or crates_built_in(rng)

    now = dt.datetime.now(dt.timezone.utc)
    transfers = [now - dt.timedelta(days=days) for days in TRANSFER_DAYS_AGO]
    last = transfers[-1]

    # Each crate starts at a random cycle, and its newer versions come in the cycles after it.
    # The cycle after the last transfer has the downloads that no manifest records yet.
    cycles = [[] for _ in range(len(transfers) + 1)]
    for name, versions in by_name.items():
        start = rng.choices(range(len(cycles)), weights=[22, 18, 18, 15, 15, 12])[0]
        for n, version in enumerate(sorted(versions, key=version_key)):
            cycles[min(start + n, len(cycles) - 1)].append((name, version))

    def since_last_transfer():
        return last + (now - last) * rng.random()

    for when, crates in zip(transfers, cycles):
        (manifests / f"{when:%Y-%m-%d}.txt").write_text(
            "".join(f"{name}@{version}\n" for name, version in sorted(crates))
        )
        for name, version in crates:
            if when == last:
                # The next cycle culls these, so they are still in the mirror
                write_crate(mirror, name, version, when - dt.timedelta(days=rng.uniform(0.2, 9)))
            elif rng.random() < DOWNLOADED_AGAIN:
                write_crate(mirror, name, version, since_last_transfer())

    for name, version in cycles[-1]:
        write_crate(mirror, name, version, since_last_transfer())

    counts = ", ".join(str(len(crates)) for crates in cycles[:-1])
    print(f"demo in {out}: manifests of {counts} crate(s), {len(cycles[-1])} un-manifested")


if __name__ == "__main__":
    main()
