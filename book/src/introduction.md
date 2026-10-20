# Introduction

zerus makes and hosts offline mirrors of crates.io. A mirror can hold only the crates
of your projects, or a larger set of crates for general use.

The usual workflow is:

1. On a computer with internet access, [download](./mirror.md) the crates with `zerus mirror`.
2. [Copy](./transfer.md) the mirror to the offline network. Record each transfer with a
   manifest, so the next transfer carries only the new crates.
3. On the offline network, [generate the index](./update-index.md) with `zerus update-index`.
4. [Serve](./serve.md) the mirror with `zerus serve`, and [point cargo at it](./cargo.md).

![The manifests page of the web UI](./images/manifests.png)
