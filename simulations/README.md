# lampo simulation harness (`simulations/`)

Pre-production soak testing for lampo on a **private regtest** bitcoind.
The harness never touches mainnet or any production / “sacred” nodes.

## Layout

| Script | Role |
|--------|------|
| `lib.sh` | Shared helpers (RPC, mining, wallet sync, artifacts) |
| `recover.sh` | Phase 1: recovery matrix + term/kill stress |
| `simulate.sh` | Phase 2: N-node soak with chaos (reorg, restart, feespam, …) |
| `multihop.sh` | Fixed `hs—hm—hr` multihop smoke |
| `run-mh.sh` | Single-instance launcher (`soak` / `recover` / `stop`) |
| `ship.sh` | Optional: git-bundle deploy to a remote build host |
| `interop.sh` / `ldk-deploy.sh` | lampo ↔ LDK-Server interop (optional) |
| `mutinet.sh` | mutinynet signet soak (optional) |
| `simln/` | SimLN activity templates (optional) |

## Sacred rules

- Host and bitcoind must be **regtest only** (default RPC `127.0.0.1:18332`).
- **Never** delete `lampod.pid` while debugging (unlinked flock = two daemons = corrupt `manager`).
- **Never** point these scripts at mainnet or production lampo data dirs.

## Prerequisites

- Built release binary: `target/release/lampod-cli` (override with `BIN=…`).
- Regtest bitcoind with RPC creds (defaults: `testutil` / `testutilpassword`).
- `bash`, `curl`, `python3`.

## Phase 1 — recovery + stress

```bash
# from repo root, after cargo build --release
export BIN=$PWD/target/release/lampod-cli
export REPO=$PWD
export SIMDIR=$PWD/sim-run-recover

SEED=99 MATRIX=1 STRESS=1 STRESS_CYCLES=25 ./simulations/recover.sh
```

Expect a final `RECOVERY COMPLETE: N PASS / 0 FAIL` line (campaign gate used `46 PASS / 0 FAIL`).

Useful knobs: `MATRIX`, `STRESS`, `STRESS_CYCLES`, `SEED`, `KEEP_GOING`, `API_BASE`, `P2P_BASE`, `CORE_URL`, `CORE_USER`, `CORE_PASS`.

## Phase 2 — N-node soak (send + receive + chaos)

Phase 2 is the **lampo edge proof**: every node must successfully **send** and
**receive** (invoice/offer/keysend), not only forward. SimLN is optional relay
load only — see `simln/README.md`.

```bash
export BIN=$PWD/target/release/lampod-cli
export REPO=$PWD
export SIMDIR=$PWD/sim-run-phase2

NODES=10 ROUNDS=20 SEED=99 CHAOS_EVERY=3 \
  API_BASE=8310 P2P_BASE=20210 \
  ./simulations/simulate.sh
```

Smoke (faster):

```bash
NODES=3 ROUNDS=2 CHAOS_EVERY=2 ./simulations/simulate.sh
```

What must pass before `SIMULATION COMPLETE`:

1. **Edge-role matrix** (`ROLE_MATRIX=1`, default): each node sends once and
   receives once; non-invoice methods covered; when the topology allows, at
   least one payment with `hops >= 2` (`MIN_MULTIHOP`, default 1).
2. Seeded soak rounds with chaos (`CHAOS_EVERY=3` + `SEED=99` hits a tip
   invalidate before round 7; harness settles before the next pay).
3. **Coverage gate**: CSV Success rows cover every node as `src` and as `dst`.

For a structural dead-end multihop path (`hs—hm—hr`), also run `multihop.sh`.

Results: `$SIMDIR/results.csv`, `$SIMDIR/sim.log`, failure artifacts under `$SIMDIR/artifacts/`.

## Remote ship (optional)

`ship.sh` pushes a git bundle to a build host that has no GitHub credentials.

```bash
export LAMPO_HOST=user@your-regtest-host   # required — no default
export LAMPO_REMOTE_DIR='$HOME/lampo-sim'
./simulations/ship.sh <branch>
```

## Agent / contributor notes

See root `AGENTS.md` and `CLAUDE.md` for how coding agents should run these gates and what not to touch.
