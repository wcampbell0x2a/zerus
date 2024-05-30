# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
