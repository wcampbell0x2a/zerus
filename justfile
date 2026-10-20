build:
    cross build --locked --workspace --target x86_64-unknown-linux-musl

test:
    -pkill python3
    rm -rf ~/.cargo/registry/index/127.0.0.1-*
    rustup install nightly-2024-05-19
    rustup +nightly-2024-05-19 component add rust-src
    rustup install nightly-2024-10-09
    rustup +nightly-2024-10-09 component add rust-src
    rustup target add x86_64-unknown-linux-musl
    cargo test --locked --workspace

# Make a demo mirror with a history of transfers in target/demo, and serve it
demo port="3000":
    python3 scripts/demo.py target/demo
    cargo run -- serve target/demo/mirror --manifests target/demo/manifests --bind 127.0.0.1:{{port}}

# Make the screenshots of the web UI for the book, from the `demo` mirror. Needs `firefox`.
book-screenshots:
    scripts/book-screenshots.sh book/src/images

# Build the book with new screenshots. Needs `mdbook`.
book: book-screenshots
    mdbook build book
