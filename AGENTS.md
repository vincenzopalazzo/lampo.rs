# AGENTS.md

Guidance for coding agents working in this repository.

## Build & Test

- `make fmt` — rustfmt (+ clippy when enabled)
- `make check` — full test suite
- `cargo check -p <crate>` / `cargo test -p <crate>` — single crate

Always run `make fmt` before committing.

## Simulation harness (`sim/`)

Pre-prod soak scripts live in `sim/`. Full usage: [`sim/README.md`](sim/README.md).

### When to run them

- After changes to shutdown, pid-lock, wallet sync, channel lifecycle, BOLT11/12 pay, or chain/reorg handling: run **Phase 1** at least.
- Before calling a release / pre-prod branch “soak-green”: run **Phase 1 + Phase 2**.
- Do **not** invent a second ad-hoc cluster; extend `sim/` instead.

### Phase 1 (recover + stress)

```bash
cargo build --release -p lampod-cli
export BIN=$PWD/target/release/lampod-cli REPO=$PWD
export SIMDIR=$PWD/sim-run-recover
SEED=99 MATRIX=1 STRESS=1 STRESS_CYCLES=25 ./sim/recover.sh
```

Gate: `RECOVERY COMPLETE: … PASS / 0 FAIL` (campaign baseline: 46/0).

### Phase 2 (N-node soak)

```bash
export BIN=$PWD/target/release/lampod-cli REPO=$PWD
export SIMDIR=$PWD/sim-run-phase2
NODES=10 ROUNDS=20 SEED=99 CHAOS_EVERY=3 \
  API_BASE=8310 P2P_BASE=20210 ./sim/simulate.sh
```

Gate: `SIMULATION COMPLETE: 20 rounds, 20 successful payments`.

Smoke: `NODES=3 ROUNDS=2 CHAOS_EVERY=2 ./sim/simulate.sh`.

### Sacred constraints (non-negotiable)

- Regtest bitcoind only (default `CORE_URL=http://127.0.0.1:18332`).
- Never delete `lampod.pid` to “unstick” a node.
- Never point `SIMDIR` / `BIN` at mainnet or production data directories.
- Never stop or reconfigure production / sacred lampo nodes from these scripts.
- Remote deploy: set `LAMPO_HOST` explicitly; `sim/ship.sh` has no default host.

### Harness design rules

- Prefer extending `lib.sh` / chaos hooks over one-off shell.
- Assert payment `state=="Success"` **and** preimage — never grep log prose.
- Wait for funding tx in mempool **before** mining.
- After tip-invalidate / reorg chaos: settle (wallet sync + payment probe) before the next pay round.
- Keep `simulate.sh` runnable standalone (soak must not depend on mid-run edits).

## Code Style

- Match existing Rust style; `cargo fmt` is mandatory.
- `unwrap` only when provably safe (`// SAFETY:`), panic = bug, or tests.
- Logging: always set `target`, prefer `debug` for routine traces.
- Imports: `std` → external → `crate::`, blank line between groups.
- Keep changes small; no drive-by refactors.

## Git & PRs

- Imperative commit subjects ≤ 50 chars, body wrapped at 72.
- Each commit must pass `make fmt` / relevant checks alone.
- No fixup commits left in a PR — squash into the offending commit.
- Optional prefixes: `cli:`, `chain:`, `node:`, `sim:`, `docs:`, `ci:`.
- Ask maintainers before adding dependencies.
