zerus
===========================

[<img alt="github" src="https://img.shields.io/badge/github-wcampbell0x2a/zerus-8da0cb?style=for-the-badge&labelColor=555555&logo=github" height="20">](https://github.com/wcampbell0x2a/zerus)
[<img alt="crates.io" src="https://img.shields.io/crates/v/zerus.svg?style=for-the-badge&color=fc8d62&logo=rust" height="20">](https://crates.io/crates/zerus)
[<img alt="docs.rs" src="https://img.shields.io/badge/docs.rs-zerus-66c2a5?style=for-the-badge&labelColor=555555&logo=docs.rs" height="20">](https://docs.rs/zerus)
[<img alt="build status" src="https://img.shields.io/github/actions/workflow/status/wcampbell0x2a/zerus/main.yml?branch=master&style=for-the-badge" height="20">](https://github.com/wcampbell0x2a/zerus/actions?query=branch%3Amaster)

Lightweight tool for creating and hosting project-specific and/or general offline crates.io mirrors.

* **Mirror**: Download the crates of your projects, or of crates by name.
* **Transfer**: Record each transfer with a manifest, and carry only the new crates the next time.
* **Serve**: Serve a sparse registry for cargo, with search and a web UI to browse the transfers.
* See the [book](https://wcampbell0x2a.github.io/zerus/) for usage, every option, and screenshots.

## Installation
Either build from published source in crates.io.
```
$ cargo install zerus --locked
```

Or download from [github releases](https://github.com/wcampbell0x2a/zerus/releases).

## Example
```console
$ zerus mirror new-mirror Cargo.toml                         # download the crates of a project
$ zerus mirror new-mirror --crate serde@1.0.210              # download a crate and its dependencies
$ zerus cull new-mirror manifests/*.txt                      # remove the crates of previous transfers
$ zerus generate-manifest new-mirror -o manifests/$(date +%F).txt  # record this transfer
$ zerus update-index new-mirror --dl-url http://[IP]         # on the offline network
$ zerus serve new-mirror --manifests manifests/              # serve the registry and the web UI
```

See the [book](https://wcampbell0x2a.github.io/zerus/) to point cargo at the mirror.

## Development
All patches/merge requests are welcome!

```
just test   # run all the tests
just demo   # serve a demo mirror at http://127.0.0.1:3000
just book   # build the book, with new screenshots of the web UI
```

## License
MIT or Apache-2.0, at your option.
