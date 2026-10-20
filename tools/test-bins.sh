#!/bin/bash
# Build the workspace test binaries once, then run them without cargo.
#
#   tools/test-bins.sh build
#   tools/test-bins.sh run-unit
#   LAMPO_CHAIN_BACKEND=core tools/test-bins.sh run-integration
#
# `run-*` uses target/ci-bins from the last `build`. CI uploads that
# directory and runs the same commands on a runner with no Rust toolchain.
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
out="$root/target/ci-bins"
cd "$root"

build() {
    mkdir -p "$out"
    set -o pipefail
    cargo test --workspace --no-run --lib --tests --message-format=json \
        >"$out/cargo-test.json"
    python3 - "$out" "$out/cargo-test.json" >"$out/manifest.tsv" <<'PY'
import json, os, shutil, sys
out = sys.argv[1]
seen = {}
raw = open(sys.argv[2], errors="replace").read()
for line in raw.splitlines():
    line = line.strip()
    if not line.startswith("{"):
        continue
    event = json.loads(line)
    if event.get("reason") != "compiler-artifact":
        continue
    exe = event.get("executable")
    if not exe:
        continue
    target = event["target"]
    kinds = target["kind"]
    if "test" not in kinds and "lib" not in kinds:
        continue
    # Cargo marks integration-test targets as kind "test". A `#[cfg(test)]`
    # lib harness is kind "lib" and its executable is `deps/<crate>-<hash>`,
    # not the `.rlib`. Skip the rlib and every plain bin.
    name = os.path.basename(exe)
    if name.endswith(".rlib") or name.endswith(".so") or name.endswith(".dylib"):
        continue
    if "test" not in kinds and "lib" not in kinds:
        continue
    key = target["name"] + "\t" + ",".join(kinds)
    seen[key] = exe
for key, exe in seen.items():
    name = os.path.basename(exe)
    dest = os.path.join(out, name)
    shutil.copy2(exe, dest)
    os.chmod(dest, 0o755)
    print(f"{key}\t{name}")
PY
    cargo build -p lampo-plugin-sdk --example lampo-bitcoind
    cp -f target/debug/examples/lampo-bitcoind "$out/lampo-bitcoind"
    chmod +x "$out/lampo-bitcoind"
    if [ ! -s "$out/manifest.tsv" ]; then
        echo "no test binaries in $out/cargo-test.json" >&2
        exit 1
    fi
}

run_matching() {
    local want=$1
    local fail=0
    local pids=()
    if [ ! -f "$out/manifest.tsv" ]; then
        echo "missing $out/manifest.tsv; run tools/test-bins.sh build" >&2
        exit 1
    fi
    while IFS=$'\t' read -r name kinds file; do
        [ -n "$name" ] || continue
        case "$want" in
            unit) [ "$name" = "tests" ] && continue ;;
            integration) [ "$name" = "tests" ] || continue ;;
        esac
        echo "run $name ($kinds)"
        "$out/$file" &
        pids+=("$!")
    done <"$out/manifest.tsv"
    if [ ${#pids[@]} -eq 0 ]; then
        echo "no $want binaries in $out/manifest.tsv" >&2
        exit 1
    fi
    for pid in "${pids[@]}"; do
        wait "$pid" || fail=1
    done
    exit "$fail"
}

run_integration() {
    export LAMPO_BITCOIND="${LAMPO_BITCOIND:-$out/lampo-bitcoind}"
    export LAMPO_CHAIN_BACKEND="${LAMPO_CHAIN_BACKEND:-core}"
    echo "chain backend: $LAMPO_CHAIN_BACKEND"
    run_matching integration
}

case "${1:-}" in
    build) build || exit $? ;;
    run-unit) run_matching unit || exit $? ;;
    run-integration) run_integration || exit $? ;;
    *)
        echo "usage: $0 build|run-unit|run-integration" >&2
        exit 2
        ;;
esac
