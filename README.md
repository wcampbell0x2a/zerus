# zerus

Lightweight binary to download only project required crates for offline crates.io mirror

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

## Serve: stable cargo
For hosting, we provide a simple server to host the git repo:
```
$ cargo build --bin zerus-serve --release
$ ./target/release/zerus-serve 192.168.42.64:80 offline-mirror/
```

### Build
For building the project that you ran `cargo vendor` for, add the following to a `.cargo/config` file(repacing IP with your ip).
```
[source.zerus]
registry = "http://[IP]/crates.io-index/"

[source.crates-io]
replace-with = "zerus"
```

## Serve: `sparse-registry` nightly cargo
You can use a simple http server:
```
$ python -m http.server 80
```

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
