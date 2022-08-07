zerus
===========================

[<img alt="github" src="https://img.shields.io/badge/github-wcampbell0x2a/zerus-8da0cb?style=for-the-badge&labelColor=555555&logo=github" height="20">](https://github.com/wcampbell0x2a/zerus)
[<img alt="crates.io" src="https://img.shields.io/crates/v/zerus.svg?style=for-the-badge&color=fc8d62&logo=rust" height="20">](https://crates.io/crates/zerus)
[<img alt="docs.rs" src="https://img.shields.io/badge/docs.rs-zerus-66c2a5?style=for-the-badge&labelColor=555555&logo=docs.rs" height="20">](https://docs.rs/zerus)
[<img alt="build status" src="https://img.shields.io/github/workflow/status/wcampbell0x2a/zerus/ci/master?style=for-the-badge" height="20">](https://github.com/wcampbell0x2a/zerus/actions?query=branch%3Amaster)

Lightweight binary to download only project required crates for offline crates.io mirror

## Requirements
Currently, this relies on nightly [sparse-registry](https://blog.rust-lang.org/2022/06/22/sparse-registry-testing.html)

## Build zerus
Either build from published source in crates.io.
```
$ cargo install zerus
```

Or download from [github releases](https://github.com/wcampbell0x2a/zerus/releases).

## Setup
Create vendor folder with all project dependencies:
```
$ cargo vendor
```

Run the following command to run this project, pointing to the `vendor` directory made in the previous step:
```
$ zerus vendor offline-mirror
```

Now clone the `crates.io-index`:
```
$ cd offline-mirror
$ git clone https://github.com/rust-lang/crates.io-index
```

## Serve mirror
Use [miniserve](https://github.com/svenstaro/miniserve).

### Build with mirror
For building the project that you ran `cargo vendor`, add the following to a `.cargo/config` file(replacing IP with your ip).
```
[unstable]
sparse-registry = true

[source.zerus]
registry = "sparse+http://[IP]/crates.io-index/"

[source.crates-io]
replace-with = "zerus"
```
