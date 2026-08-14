# lampo simulation harness (`sim/`)

Pre-production soak testing on the `vincenzopalazzo@debian` server.
Full plan & research: `docs/simulation/2026-08-14-simulation-plan.md`.

## What runs where

- **Mac** (this worktree `lampo-sim-main`): code, branches, PRs, `sim/ship.sh`.
- **debian server**: `~/lampo-sim` (clone fed by git bundles), release build,
  regtest cluster `n1..n6` (API `:8101..`, P2P `:19901..`, data `~/lampo-sim/sim-run/<n>`),
  `~/lampo-sim/sim-run/sim.log` + `results.csv` + `artifacts/`.
- regtest backend: existing `spark-bitcoind-1` (host RPC `127.0.0.1:18332`).
  Existing `node-s/m/r` and production nodes are **never touched**.

## First deployment (one-time)

```bash
# from the worktree on the Mac
./sim/ship.sh sim/main            # ships bundle, clones ~/lampo-sim, starts build
ssh vincenzopalazzo@debian 'tail -f ~/lampo-sim/build.log'   # wait for BUILD_OK
```

## Running the simulation (on the server)

```bash
ssh vincenzopalazzo@debian
cd ~/lampo-sim
# quick smoke: 3 nodes, 2 payment rounds, 1 chaos event
NODES=3 ROUNDS=2 CHAOS_EVERY=2 ./sim/simulate.sh
# endless soak under screen/tmux
ROUNDS=0 SEED=1337 ./sim/simulate.sh
```

Key env vars: `NODES ROUNDS(0=endless) SEED PAY_MIN_MSAT PAY_MAX_MSAT
CHAOS_EVERY METHODS TMO KEEP_GOING` — see the plan doc for the full table.

Chaos events (seeded): `restart9` (SIGKILL+relaunch), `storm` (block storm),
`reorg` (invalidate+fork), `feespam`, `churn` (close+reopen channel),
`zapconn` (kill TCP, auto-reconnect must repair).

## Bug → PR → rebuild → retest loop

1. Failure ⇒ artifacts in `~/lampo-sim/sim-run/artifacts/<ts>-*/` (node dirs,
   logs, `results.csv`, mempool + getinfo snapshots). Fetch them:
   `scp -r vincenzopalazzo@debian:~/lampo-sim/sim-run/artifacts/<ts>-* .`
2. On the Mac, in this worktree: `git checkout -b fix/<slug>`, fix, commit, push, open PR.
3. `./sim/ship.sh fix/<slug>` — bundle → server fetch → checkout → `cargo build --release`
   (watch `build.log` for `BUILD_OK`).
4. Regression: rerun with the **same SEED**:
   `cd ~/lampo-sim && NODES=3 ROUNDS=2 CHAOS_EVERY=2 ./sim/simulate.sh`
   then resume the endless soak.
5. PR merged ⇒ `git fetch origin && git checkout sim/main && git rebase origin/main`
   ⇒ `./sim/ship.sh sim/main` ⇒ keep soaking on updated main.

## Regression assertions every run

- multi-inbound peers per node (peer-manager fix)
- fundchannel with `public:true` honored
- multi-hop bolt11 payment: `state == "Success"` **and** preimage
- multi-hop BOLT12 offer payment (after 150 s gossip propagation)
- auto-reconnect after `zapconn`/`restart9` chaos, followed by a payment probe
- no `panic|corrupt|invariant` in any node log
