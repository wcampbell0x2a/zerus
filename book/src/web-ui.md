# Web UI

`zerus serve` shows a web UI at the root path. Set `--manifests` to the directory of the
manifest files from `generate-manifest`, to see the crates of each transfer.
```console
$ zerus serve new-mirror --manifests manifests/
```

The manifests page shows each manifest, with the number of its crates that are in the
mirror and the number that `cull` removed. The *un-manifested* row has the crates that
are in the mirror and in no manifest, which are the crates for the next transfer.

![The manifests page](./images/manifests.png)

A manifest page shows its crates, with the time that `zerus mirror` wrote each one. Click
a column header to sort, and click it again to reverse the sort.

![A manifest page](./images/manifest.png)

The un-manifested page shows the crates for the next transfer.

![The un-manifested page](./images/unmanifested.png)

Search finds a crate in all the manifests and in the mirror, and shows if `cull`
removed it. Press `s` to go to the search box.

![The search page](./images/search.png)

## Scans

The server scans the mirror and the manifests in the background when it starts, and
keeps the result in memory. Thus the pages stay fast when the mirror is on a slow disk
or a network disk. Cargo can use the registry while the first scan runs.

The header of each page shows the age of its data. When a page loads and its data is
older than 10 minutes, the server scans again in the background. The page shows the old
data until the scan is done. A server that nobody looks at does no scans. To scan
immediately, click Refresh in the header. The page loads again when the scan is done.
Only one scan runs at a time.

To set the age, use `--rescan-after`, for example `30s` or `1h`. Use `0` so that only
Refresh starts a scan:
```console
$ zerus serve new-mirror --manifests manifests/ --rescan-after 1h
```

## Try it

`just demo` makes a demo mirror with five transfers in `target/demo`, and serves it at
`http://127.0.0.1:3000`. To use a different port, use `just demo 8000`. The screenshots
on this page come from this demo.
