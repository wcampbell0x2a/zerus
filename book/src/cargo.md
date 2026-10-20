# Build With the Mirror

Add this to `.cargo/config.toml`. Replace `[IP]` with the server address.
```toml
[source.zerus]
registry = "sparse+http://[IP]/crates.io-index/"

[source.crates-io]
replace-with = "zerus"

[registries.zerus]
index = "sparse+http://[IP]/crates.io-index/"
```

## Search

With the `registries` entry, you can search the mirror:
```console
$ cargo search --registry zerus serde
```

The server reads the index for the search when it starts, and keeps it in memory. Each
run of `update-index` writes a marker file, `crates.io-index/.zerus-updated`. Thus the next
search reads the index again and finds the new crates. If the index has no marker, for
example from an older zerus, the search reads the index again when it is older than
`--rescan-after`.
