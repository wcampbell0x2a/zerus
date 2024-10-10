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
