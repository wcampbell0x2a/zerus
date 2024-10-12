zerus
===========================

[<img alt="github" src="https://img.shields.io/badge/github-wcampbell0x2a/zerus-8da0cb?style=for-the-badge&labelColor=555555&logo=github" height="20">](https://github.com/wcampbell0x2a/zerus)
[<img alt="crates.io" src="https://img.shields.io/crates/v/zerus.svg?style=for-the-badge&color=fc8d62&logo=rust" height="20">](https://crates.io/crates/zerus)
[<img alt="docs.rs" src="https://img.shields.io/badge/docs.rs-zerus-66c2a5?style=for-the-badge&labelColor=555555&logo=docs.rs" height="20">](https://docs.rs/zerus)
[<img alt="build status" src="https://img.shields.io/github/actions/workflow/status/wcampbell0x2a/zerus/main.yml?branch=master&style=for-the-badge" height="20">](https://github.com/wcampbell0x2a/zerus/actions?query=branch%3Amaster)

Lightweight binary to download only project required crates for offline crates.io mirror

## Build zerus
Either build from published source in crates.io.
```
$ cargo install zerus --locked
```

Or download from [github releases](https://github.com/wcampbell0x2a/zerus/releases).

## Usage
```console
Usage: zerus [OPTIONS] <MIRROR_PATH> [WORKSPACES]...

Arguments:
  <MIRROR_PATH>    new directory to contain offline mirror crate files
  [WORKSPACES]...  list of Cargo.toml files to vendor depends

Options:
      --build-std <VERSION>  Cache build-std depends for nightly version
      --skip-git-index       Skip download of git index crates.io
  -h, --help                 Print help
```

Example:
```console
$ zerus new-mirror ../deku/Cargo.toml ../adsb_deku/Cargo.toml
# configure crates.io-index to point to our host
$ cat crates.io-index/config.json
{
  "dl": "http://[IP]/crates/{prefix}/{crate}/{version}/{crate}-{version}.crate",
  "api": "http://[IP]/crates"
}
```


## Serve mirror
Use [miniserve](https://github.com/svenstaro/miniserve).

### Build with mirror
Add the following to the `.cargo/config` file(replacing IP with your ip).
```
[source.zerus]
registry = "sparse+http://[IP]/crates.io-index/"

[source.crates-io]
replace-with = "zerus"
```
