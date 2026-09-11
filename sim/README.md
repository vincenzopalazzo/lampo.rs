# lampo simulation harness (`sim/`)

Pre-production soak testing on the `vincenzopalazzo@debian` server.
Full plan & research: `docs/simulation/2026-08-15-main-next-simulation-plan.md`
(earlier plan: `docs/simulation/2026-08-14-simulation-plan.md`).

## Tiers

| Tier | What | Server clone | Ports |
|---|---|---|---|
| 1 | lampo-only soak + multihop + recovery (`sim/test-573-577`) | `~/lampo-sim` | 8101+/19901+, mh 8211-13/20111-13 |
| 2 | **main-next soak** — updated `origin/main` | `~/lampo-main-sim` | 8301+/20101+ |
| 3 | **interop** lampo ↔ LDK-Server (`interop.sh`) | `~/lampo-main-sim` | lampo 8321-22/20121-22, ldk 3541-42 gRPC + 9841-42 P2P |
| 4 | SimLN realistic activity (`simln/`) | same cluster | ldk edges 3543+ |
| 5 | mutinynet signet (`mutinet.sh`) | `~/lampo-sim` m1/m2 | — |

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

## Tier 2/3: main-next soak + LDK-Server interop (one-time)

```bash
# from the lampo-main-sim worktree on the Mac
LAMPO_REMOTE_DIR='$HOME/lampo-main-sim' ./sim/ship.sh sim/main-next
ssh vincenzopalazzo@debian 'tail -f ~/lampo-main-sim/build.log'  # BUILD_OK

# on the server: ldk-server nodes for the interop tier
ssh vincenzopalazzo@debian
~/lampo-main-sim/sim/ldk-deploy.sh build   # clone + protoc + cargo build
~/lampo-main-sim/sim/ldk-deploy.sh start 2 # lk1 (gRPC :3541), lk2 (:3542)
~/lampo-main-sim/sim/interop.sh            # I01..I14 cross-impl assertions
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

## Multi-hop harness (`sim/multihop.sh`)

Dedicated dead-end topology `hs — hm — hr` (hs and hr share NO channel, so
every hs↔hr payment structurally requires ≥2 hops through relay `hm`).
Ports 8211-8213 / 20111-20113, data `$SIMDIR/mh/` — runs in parallel with
the endless soak.

```bash
./sim/multihop.sh                  # build-or-reuse cluster, assert, stress
ENSURE_ONLY=1 ./sim/multihop.sh    # just make sure the cluster exists
MH_ROUNDS=0 SEED=7 ./sim/multihop.sh   # endless multihop soak
```

Assertions beyond simulate.sh: `hops >= 2`, relay `hm` present on the
payment path, both directions, offer + keysend over the 2-hop route, and a
relay-bounce (SIGKILL hm → restart → pay again). Every row in
`$SIMDIR/mh/results.csv` records the hop count (closing the old TODO of
never asserting path length).

## Recovery matrix + stress (`sim/recover.sh`)

Runs against the multihop cluster (reuses it, or builds it) and answers:
"something bad happened — is the LN state restored, and are the funds
safe?" Expected behaviors verified against LDK 0.3 source
(`ln/channelmanager.rs`):

| Case | Fault | Expected |
|---|---|---|
| R01-R03 | SIGINT / SIGTERM / SIGKILL idle | full restore, channels ready, probe pays |
| R04-R06 | SIGKILL mid-payment (payer/relay/payee) | restore; interrupted payment fails or settles cleanly |
| R07 | 40 blocks mined while node down | catch up via wallet sync, channels intact |
| R08 | all nodes down at once | all restore |
| R09 | peer force-closes while we're down | channel closed on restart, node sane, repairable |
| R10 | stale `manager` (old backup, monitors ahead) | LDK force-closes from monitor state; NEVER fully usable |
| R11 | corrupt `manager` bytes | fail fast, API never up, file left untouched |
| R12 | missing `monitors/` | fail fast (never run channels without monitors) |
| R13 | corrupt single monitor file | fail fast, file untouched |
| R14 | stale monitor (manager ahead) | fail fast (`DecodeError::DangerousValue` path) |

After every case: same `node_id`, same channel set, channels ready ≤180 s,
2-hop probe payment `Success`+preimage, no `panic|corrupt|invariant` in
the log delta. A randomized stress loop (`STRESS_CYCLES`, 0 = endless)
then mixes kill/term mid-payment, chain-advance-while-down and double
kills under a money guard: the cluster-wide channel-balance total may
neither drop >1% (funds vanishing) nor grow beyond the fee budget
(money printing).

```bash
./sim/recover.sh                       # matrix + 25 stress cycles
STRESS_CYCLES=0 SEED=99 ./sim/recover.sh   # endless recovery soak
MATRIX=0 STRESS=1 ./sim/recover.sh     # stress only
```
