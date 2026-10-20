# Generate the Index

On the offline network, use `update-index` to generate a registry index from the
`.crate` files.
```console
$ zerus update-index new-mirror --dl-url http://[IP]
```

`--dl-url` is a base URL. zerus adds
`/crates/{prefix}/{crate}/{version}/{crate}-{version}.crate` to it. Give the server
address, not the full download template.

Run `update-index` again each time you add crates to the mirror.
