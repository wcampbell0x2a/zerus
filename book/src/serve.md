# Serve the Mirror

Use `zerus serve` to serve the registry. It serves a sparse index, the crate downloads,
a search API, and a [web UI](./web-ui.md).
```console
$ zerus serve new-mirror --bind 0.0.0.0:8080
```

To log the requests:
```console
$ RUST_LOG=tower_http=debug zerus serve new-mirror
```

## Serve with Docker

Each release pushes an image that runs `zerus serve`. Mount the mirror at `/mirror`.
```console
$ docker run -p 8080:8080 -v "$PWD/new-mirror:/mirror:ro" ghcr.io/wcampbell0x2a/zerus
```

To change the command, add arguments. For example, to see the previous manifests:
```console
$ docker run -p 8080:8080 -v "$PWD/new-mirror:/mirror:ro" ghcr.io/wcampbell0x2a/zerus \
      serve /mirror --bind 0.0.0.0:8080 --manifests /mirror/manifests
```

Use `latest`, or a version tag such as `ghcr.io/wcampbell0x2a/zerus:0.16.0`.
