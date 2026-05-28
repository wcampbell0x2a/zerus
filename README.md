zerus
===========================

[<img alt="github" src="https://img.shields.io/badge/github-wcampbell0x2a/zerus-8da0cb?style=for-the-badge&labelColor=555555&logo=github" height="20">](https://github.com/wcampbell0x2a/zerus)
[<img alt="crates.io" src="https://img.shields.io/crates/v/zerus.svg?style=for-the-badge&color=fc8d62&logo=rust" height="20">](https://crates.io/crates/zerus)
[<img alt="docs.rs" src="https://img.shields.io/badge/docs.rs-zerus-66c2a5?style=for-the-badge&labelColor=555555&logo=docs.rs" height="20">](https://docs.rs/zerus)
[<img alt="build status" src="https://img.shields.io/github/actions/workflow/status/wcampbell0x2a/zerus/main.yml?branch=master&style=for-the-badge" height="20">](https://github.com/wcampbell0x2a/zerus/actions?query=branch%3Amaster)

Lightweight tool for creating project-specific and/or general offline crates.io mirrors

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

### Generate index
On the offline network, use `update-index` to generate a registry index from the `.crate` files.
```console
$ zerus update-index new-mirror --dl-url http://[IP]
```

### Serve mirror
Use `zerus serve` to host the registry with sparse index, crate downloads, and a search API.
```console
$ zerus serve new-mirror --bind 0.0.0.0:8080
```

Enable request logging with:
```console
$ RUST_LOG=tower_http=debug zerus serve new-mirror
```

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
