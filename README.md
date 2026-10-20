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

### Download crates
Use `zerus mirror` to download `.crate` files for your project's dependencies.
```console
$ zerus mirror new-mirror ../deku/Cargo.toml ../adsb_deku/Cargo.toml
```

Mirror individual crates by name (resolves to the latest version) or pinned to a specific version. When `--crate` is used, `--get-feature-gated` is implied.
```console
$ zerus mirror new-mirror --crate reqwest --crate serde@1.0.210
```

Use `--get-feature-gated` to recursively expand and download all transitive dependencies, regardless of which features are currently enabled. This is useful when building a more complete mirror rather than one tailored to a specific project's current feature set — ensuring crates are available even if features change later.

> [!NOTE]
> The expansion (run by `--get-feature-gated` and implied by `--crate`) includes dev and build dependencies of every crate it visits and ignores feature gating, so the result is far larger than what `cargo metadata` would resolve — even a tiny crate like `cfg-if` expands to thousands of crates. To mirror only a project's resolved dependencies, pass its `Cargo.toml` instead of using `--crate`.
```console
$ zerus mirror new-mirror --get-feature-gated ../deku/Cargo.toml
```

Adding the top 100 rust crates used by rust-playground is easy:
```console
$ git clone https://github.com/rust-lang/rust-playground
$ zerus mirror new-mirror rust-playground/top-crates/Cargo.toml
```

### Transfer to offline network
Copy the mirror directory to your proxy or offline network.

### Repeat transfers
To avoid carrying crates that already made a previous trip, record each transfer with
`generate-manifest` and use `cull` to drop already-transferred crates before the next one.
```console
$ zerus mirror new-mirror Cargo.toml                        # download everything
$ zerus cull new-mirror manifests/*.txt                     # remove crates from previous manifests
$ zerus generate-manifest new-mirror -o manifests/$(date +%F).txt   # record this transfer
# copy new-mirror to the offline network, keep manifests/ locally
```
`cull` takes any number of manifest files and removes the union of their entries; pass
`--dry-run` to preview what would be deleted. Run `update-index` on the offline network
after merging the new crates into the existing mirror.

### Generate index
On the offline network, use `update-index` to generate a registry index from the `.crate` files.
```console
$ zerus update-index new-mirror --dl-url http://[IP]
```

The URL is a base path — zerus appends `/crates/{prefix}/{crate}/{version}/{crate}-{version}.crate` to it. Pass the server address, not the full download template.

### Serve mirror
Use `zerus serve` to host the registry with sparse index, crate downloads, and a search API.
```console
$ zerus serve new-mirror --bind 0.0.0.0:8080
```

Enable request logging with:
```console
$ RUST_LOG=tower_http=debug zerus serve new-mirror
```

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

Use `latest`, or pin to a version tag such as `ghcr.io/wcampbell0x2a/zerus:0.16.0`.

#### Browse past manifests
Point `--manifests` at the directory of manifest files written by `generate-manifest` to
browse them in the web UI at the root path.
```console
$ zerus serve new-mirror --manifests manifests/
```

The server scans the mirror and the manifests in the background when it starts, and keeps
the result in memory, so the pages stay fast when the mirror is on a slow or network disk.
Cargo can use the registry while the first scan runs. The header of each page shows how old
its data is. When a page loads and its data is older than 10 minutes, the server scans again in
the background, and the page shows the old data until the scan is done. A server that nobody
looks at does no scans. To scan at once, click Refresh in the header. The page reloads when the
scan is done. Only one scan runs at a time.

Set the age with `--rescan-after`, e.g. `30s` or `1h`. Use `0` so that only Refresh scans:
```console
$ zerus serve new-mirror --manifests manifests/ --rescan-after 1h
```

To try the web UI, `just demo` makes a demo mirror with five transfers in `target/demo` and
serves it at `http://127.0.0.1:3000`. Pass another port with `just demo 8000`.

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
