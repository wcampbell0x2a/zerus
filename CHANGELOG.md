# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.10.0] - 10-21-2024
- Add instructions for use with [integer32llc/Margo](https://github.com/integer32llc/margo)
- Add `--git-index-url` to configure mirror `config.json` [#95](https://github.com/wcampbell0x2a/zerus/pull/95)
- Add check of status code when downloading [#97](https://github.com/wcampbell0x2a/zerus/pull/97)
- Add better message for not finding `Cargo.toml` [#96](https://github.com/wcampbell0x2a/zerus/pull/96)

### Dependencies
- Bump clap from 4.5.9 to 4.5.20
- Bump git2 from 0.18.3 to 0.19.0
- Bump anyhow from 1.0.82 to 1.0.90
- Bump reqwest from 0.12.3 to 0.12.8

## [0.9.0] - 10-12-2024
- Add support for latest nightly
- Add `--skip-git-index` to not download git index crates.io

## [0.8.1] - 05-30-2024
- Force checkout when using Fast-Forward when updating git `crates.io-index`

## [0.8.0] - 05-29-2024
- Properly set local HEAD to fetched git repo `crates.io-index` when updating from previous zerus invocation

## [0.7.0] - 04-11-2024
- Add automatic crates.io git clone and fetch for mirror

## [0.6.0] - 02-25-2024
- Download crates in parallel

## [0.5.0] - 02-03-2024
- Support vendoring rustc build-std dependencies for specific nightly versions with `zerus --build-std`
- Improve performance
- Update README.md

## [0.4.0] - 02-02-2023
- Update example usage of `spare-registry` in readme to latest stabilized config
- Fix path of crates of filename length 3
- Add example `config.json` to README

## [0.3.0] - 12-30-2022
- Deprecate need for `cargo vendor`, instead resolving depends from Cargo.toml files ourself using `guppy`.

## [0.2.0] - 08-07-2022
- Deprecate `serve`, we only support the nightly feature `sparse-registry`

## [0.1.0] - 07-27-2022
- Initial Release
