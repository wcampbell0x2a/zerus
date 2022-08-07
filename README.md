# zerus

Lightweight binary to download only project required crates for offline crates.io mirror

## Requirements
Currently, this relies on nightly [sparse-registry](https://blog.rust-lang.org/2022/06/22/sparse-registry-testing.html)

## Setup
Create vendor folder with all project dependencies:
```
$ cargo vendor
```

Run the following command to run this project, pointing to the `vendor` directory made in the previous step:
```
$ cargo r --release --bin zerus -- vendor offline-mirror
```

Now clone the `crates.io-index`:
```
$ cd offline-mirror
$ git clone https://github.com/rust-lang/crates.io-index
```

## Serve: `sparse-registry` nightly cargo

### Build
For building the project that you ran `cargo vendor` for, add the following to a `.cargo/config` file(repacing IP with your ip).
```
[unstable]
sparse-registry = true

[source.zerus]
registry = "sparse+http://[IP]/crates.io-index/"

[source.crates-io]
replace-with = "zerus"
```
