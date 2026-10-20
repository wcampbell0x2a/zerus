#!/usr/bin/env bash
# Make the screenshots of the web UI for the book.
#
# Makes the demo mirror of `just demo`, serves it, and takes screenshots with headless
# Firefox. The demo dates are relative to today, so the manifest names change each run.
#
# Usage: book-screenshots.sh <output dir> [port]
set -euo pipefail

out=${1:?usage: book-screenshots.sh <output dir> [port]}
port=${2:-3917}
url="http://127.0.0.1:${port}"
demo=target/demo

mkdir -p "$out"
# Firefox does not resolve a relative screenshot path from this directory
out=$(realpath "$out")
python3 scripts/demo.py "$demo"
cargo build --locked --quiet

cargo run --locked --quiet -- serve "$demo/mirror" --manifests "$demo/manifests" \
    --bind "127.0.0.1:${port}" &
server=$!
trap 'kill "$server" 2>/dev/null || true' EXIT

for _ in $(seq 100); do
    curl --silent --fail "$url/" >/dev/null && break
    sleep 0.1
done
curl --silent --fail "$url/" >/dev/null

# The first scan runs in the background, so wait until the pages show its data
for _ in $(seq 100); do
    curl --silent "$url/" | grep --quiet 'href="/manifests/' && break
    sleep 0.1
done

# The second-newest manifest has crates in the mirror and culled crates
manifest=$(curl --silent "$url/" | grep --only-matching 'href="/manifests/[^"]*"' |
    sed --quiet '2s/^href="\(.*\)"$/\1/p')

shoot() {
    local name=$1 path=$2
    local profile
    profile=$(mktemp --directory)
    rm -f "$out/$name.png"
    # Take the screenshots in the dark mode of the web UI
    cat >"$profile/user.js" <<'EOF'
user_pref("ui.systemUsesDarkTheme", 1);
user_pref("layout.css.prefers-color-scheme.content-override", 0);
EOF
    firefox --headless --no-remote --profile "$profile" --window-size=1280,800 \
        --screenshot "$out/$name.png" "$url$path" >/dev/null 2>&1
    rm -rf "$profile"
    if [[ ! -s "$out/$name.png" ]]; then
        echo "error: firefox did not write $out/$name.png" >&2
        exit 1
    fi
    echo "$out/$name.png"
}

shoot manifests /
shoot manifest "$manifest"
shoot unmanifested /unmanifested
shoot search '/search?q=serde'
