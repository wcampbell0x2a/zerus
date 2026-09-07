zerus
===========================

[<img alt="github" src="https://img.shields.io/badge/github-wcampbell0x2a/zerus-8da0cb?style=for-the-badge&labelColor=555555&logo=github" height="20">](https://github.com/wcampbell0x2a/zerus)
[<img alt="crates.io" src="https://img.shields.io/crates/v/zerus.svg?style=for-the-badge&color=fc8d62&logo=rust" height="20">](https://crates.io/crates/zerus)
[<img alt="docs.rs" src="https://img.shields.io/badge/docs.rs-zerus-66c2a5?style=for-the-badge&labelColor=555555&logo=docs.rs" height="20">](https://docs.rs/zerus)
[<img alt="build status" src="https://img.shields.io/github/actions/workflow/status/wcampbell0x2a/zerus/main.yml?branch=master&style=for-the-badge" height="20">](https://github.com/wcampbell0x2a/zerus/actions?query=branch%3Amaster)

Lightweight tool for creating and hosting project-specific and/or general offline crates.io mirrors

## Build zerus
Either build from published source in crates.io.
```
$ cargo install zerus --locked
```

Or download from [github releases](https://github.com/wcampbell0x2a/zerus/releases).

## Usage

Two commands carry the whole workflow: `pack` on the connected side, and `serve` on the
offline side. Each transfer is one file that you upload to the running server.

```console
# connected side: download, take what has not gone across yet, record the transfer
$ zerus pack new-mirror Cargo.toml --manifests manifests/
  wrote transfers/2026-09-06.zpk (412 crate(s), 61.2 MiB)

# offline side: host the registry, and accept packs
$ zerus serve /mirror --manifests /mirror/manifests \
      --upload-token "$TOKEN" --dl-url http://[IP]
```

Carry the `.zpk` across, then open `http://[IP]/uploads` in a browser and send it with the
form. The crates merge into the mirror, the index is rebuilt, and `cargo` sees them without
a restart.

### Pack a transfer
`zerus pack` downloads your dependencies, leaves out what earlier transfers already carried,
and records this one. It writes a single pack file: a magic header followed by a zstd
SquashFS image holding the crates and a manifest of what is inside.
```console
$ zerus pack new-mirror Cargo.toml --manifests manifests/
```
Pass any number of `Cargo.toml` files, or name crates directly. `--crate` resolves to the
latest version unless you pin one, and implies `--get-feature-gated`.
```console
$ zerus pack new-mirror ../deku/Cargo.toml ../adsb_deku/Cargo.toml --manifests manifests/
$ zerus pack new-mirror --crate reqwest --crate serde@1.0.210 --manifests manifests/
```

The pack goes to `transfers/<today>.zpk` unless `-o` says otherwise. A second pack on the
same day gets a counter rather than overwriting the first. The transfer is recorded in the
`--manifests` directory, named after the pack: `2026-09-06.zpk` is recorded as
`manifest-2026-09-06.txt`.

`--manifests` is required, and there is no default: the directory is what tells `pack` which
crates already went across, and a path relative to the current directory would point at a
different record from a different directory. Give the same path every time. To pack without
a record, for a transfer that may not happen, pass `--no-record` instead.

Because crates recorded in `manifests/` are left out, each pack holds only what is new. The
mirror itself keeps everything, so nothing is lost between transfers.

> [!NOTE]
> `.crate` files are already gzip-compressed, so a pack is close to the size of the crates it
> holds. The container is for carrying one self-describing file, not for saving space.

Use `--get-feature-gated` to expand and download all transitive dependencies, whatever
features are enabled now. This builds a mirror that stays usable when features change later.

> [!NOTE]
> The expansion (run by `--get-feature-gated` and implied by `--crate`) includes dev and build dependencies of every crate it visits and ignores feature gating, so the result is far larger than what `cargo metadata` would resolve — even a tiny crate like `cfg-if` expands to thousands of crates. To mirror only a project's resolved dependencies, pass its `Cargo.toml` without using `--crate`.

Adding the top 100 rust crates used by rust-playground is easy:
```console
$ git clone https://github.com/rust-lang/rust-playground
$ zerus pack new-mirror rust-playground/top-crates/Cargo.toml --manifests manifests/
```

To pack a mirror that is already on disk, with no download step:
```console
$ zerus pack-from-mirror new-mirror --manifests manifests/
```

### Serve the mirror
`zerus serve` hosts the registry with a sparse index, crate downloads, a search API, and the
web UI. The web UI opens on a search box, with `manifests` and `uploads` in the header.
```console
$ zerus serve new-mirror --bind 0.0.0.0:8080
```

Enable request logging with:
```console
$ RUST_LOG=tower_http=debug zerus serve new-mirror
```

Point `--manifests` at the directory of manifest files to browse past transfers at
`/manifests`. Uploads record themselves there too, so pass it if you accept uploads.
```console
$ zerus serve new-mirror --manifests manifests/
```

#### Upload packs to a running server
Give `serve` an upload token to accept packs at `/uploads` in the browser, or at
`POST /admin/upload` for scripts. An upload merges into the mirror and updates the index, and
the server reads both from disk on each request, so new crates resolve without a restart.
```console
$ zerus serve /mirror --manifests /mirror/manifests \
      --upload-token "$TOKEN" --dl-url http://[IP]
$ curl -X POST -H "Authorization: Bearer $TOKEN" \
      -F pack=@transfers/2026-09-06.zpk http://[IP]/admin/upload
{"added":2,"skipped":3,"crates":["phf@0.9.0","png@0.17.16"]}
```
The token can come from `ZERUS_UPLOAD_TOKEN` instead of the command line. In the browser, the
`/uploads` page takes the pack file and the token in a form, reports what the merge added,
and lists the uploads the server has accepted since it started. A browser gets a page back;
`curl` and scripts get the JSON above.

A crate the mirror already holds is left alone, so re-sending a pack is safe. With
`--manifests`, each upload records the transfer, giving the offline side the same record the
sending side kept; without it the upload still merges, but its crates show as un-manifested.

> [!WARNING]
> Without `--upload-token` the endpoint does not exist and the server stays read-only. The
> token crosses the network in the clear, so put this behind TLS or keep it on a trusted
> network.

#### Serve with docker
Each release pushes an image that runs `zerus serve`. Mount the mirror at `/mirror`.
```console
$ docker run -p 8080:8080 -v "$PWD/new-mirror:/mirror:ro" ghcr.io/wcampbell0x2a/zerus
```

Add arguments to replace the default command, for example to browse past manifests:
```console
$ docker run -p 8080:8080 -v "$PWD/new-mirror:/mirror:ro" ghcr.io/wcampbell0x2a/zerus \
      serve /mirror --bind 0.0.0.0:8080 --manifests /mirror/manifests
```

To accept uploads, drop the `:ro` so the container can write to the mirror:
```console
$ docker run -p 8080:8080 -v "$PWD/new-mirror:/mirror" \
      -e ZERUS_UPLOAD_TOKEN="$TOKEN" ghcr.io/wcampbell0x2a/zerus \
      serve /mirror --bind 0.0.0.0:8080 --manifests /mirror/manifests \
      --dl-url http://[IP]
```

Use `latest`, or pin to a version tag such as `ghcr.io/wcampbell0x2a/zerus:0.16.0`.

### Build with mirror
Add the following to `.cargo/config.toml` (replacing `[IP]` with your server address).
```toml
[source.zerus]
registry = "sparse+http://[IP]/crates.io-index/"

[source.crates-io]
replace-with = "zerus"

[registries.zerus]
index = "sparse+http://[IP]/crates.io-index/"
```

With the `registries` entry, you can search the mirror:
```console
$ cargo search --registry zerus serde
```

## Individual commands
`pack` and `serve` cover the usual workflow. These commands do the same work a step at a
time, for a transfer that does not go over HTTP, or when you want to drive each step
yourself.

### Merge a pack from the command line
`unpack` is the `/uploads` page without the server: it merges a pack into a mirror and
updates the index. Use it when the server is not running, or the pack is too big to push
through HTTP.
```console
$ zerus unpack /mirror transfers/2026-09-06.zpk --dl-url http://[IP]
```
A crate the mirror already holds is left alone, so re-applying a pack is safe and does not
rebuild the index. Pass `--dry-run` to list what a pack holds without writing anything.

The transfer is recorded in `<mirror>/manifests/` by default, the same as an upload. Point
`--manifests` elsewhere, or pass `--no-record` to skip it. Unlike `pack`, `unpack` can
default the directory safely, because it hangs off the mirror path you already gave it and
so means the same thing from any directory.

### Download without packing
`zerus mirror` downloads `.crate` files into a mirror directory and stops there. It takes the
same arguments as `pack`.
```console
$ zerus mirror new-mirror ../deku/Cargo.toml
$ zerus mirror new-mirror --crate reqwest --crate serde@1.0.210
$ zerus mirror new-mirror --get-feature-gated ../deku/Cargo.toml
```
Copy the mirror directory to the offline network yourself, then run `update-index` there.

### Generate the index
`update-index` builds a registry index from the `.crate` files in a mirror. `unpack` and
uploads run it for you; run it by hand after copying crates in some other way.
```console
$ zerus update-index new-mirror --dl-url http://[IP]
```

### Track transfers by hand
`cull` and `generate-manifest` are what `pack` uses internally, as separate steps. `cull`
deletes already-transferred crates from the mirror, where `pack` instead leaves the mirror
whole and takes only the new crates.
```console
$ zerus mirror new-mirror Cargo.toml                        # download everything
$ zerus cull new-mirror manifests/*.txt                     # remove crates from previous manifests
$ zerus generate-manifest new-mirror -o manifests/$(date +%F).txt   # record this transfer
```
`cull` takes any number of manifest files and removes the union of their entries; pass
`--dry-run` to preview what would be deleted.
