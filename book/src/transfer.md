# Transfer to an Offline Network

Copy the mirror directory to your proxy or offline network.

## Repeat transfers

Do not carry crates that a previous transfer carried. Record each transfer with
`generate-manifest`, and use `cull` to remove the crates of previous transfers before
the next one.
```console
$ zerus mirror new-mirror Cargo.toml                                # download all crates
$ zerus cull new-mirror manifests/*.txt                             # remove the crates of previous transfers
$ zerus generate-manifest new-mirror -o manifests/$(date +%F).txt   # record this transfer
```

Copy `new-mirror` to the offline network, and keep `manifests/` on this side.

`cull` takes one or more manifest files, and removes all the crates in them. To see
what it removes, and not remove it, use `--dry-run`.

On the offline network, merge the new crates into the mirror, then
[generate the index](./update-index.md) again.

To see the manifests in a browser, see [Web UI](./web-ui.md).
