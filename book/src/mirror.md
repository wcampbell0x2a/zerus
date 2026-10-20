# Download Crates

Use `zerus mirror` to download the `.crate` files for the dependencies of your projects.
```console
$ zerus mirror new-mirror ../deku/Cargo.toml ../adsb_deku/Cargo.toml
```

To mirror a crate by name, use `--crate`. A name alone gives the latest version, and
`name@version` gives that version. `--crate` implies `--get-feature-gated`.
```console
$ zerus mirror new-mirror --crate reqwest --crate serde@1.0.210
```

## All dependencies, for all features

Use `--get-feature-gated` to download all the dependencies of each crate, recursively,
for all features. The mirror then has the crates that your projects need if you enable
more features later.
```console
$ zerus mirror new-mirror --get-feature-gated ../deku/Cargo.toml
```

> [!NOTE]
> This expansion includes the dev and build dependencies of each crate, and ignores
> feature gates. Thus the result is much larger than what `cargo metadata` resolves. Even
> a small crate such as `cfg-if` expands to thousands of crates. To mirror only the
> resolved dependencies of a project, give its `Cargo.toml` and do not use `--crate`.

## The top crates

To add the top 100 crates of the Rust Playground:
```console
$ git clone https://github.com/rust-lang/rust-playground
$ zerus mirror new-mirror rust-playground/top-crates/Cargo.toml
```
