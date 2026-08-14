# Lampo Pre-Production Simulation Plan

**Date:** 2026-08-14
**Status:** Active
**Worktree:** `lampo-workspace/lampo-sim-main` (branch `sim/main`, tracks `origin/main`)
**Target host:** `vincenzopalazzo@debian` (8 cores, 15 GB RAM, ~900 GB free)

---

## 1. Goal

Find bugs in lampo **before production** by running a continuous, chaos-injected
multi-hop simulation on the debian server, on **regtest** (primary) and
**signet/mutinynet** (secondary), against binaries built from an up-to-date
`main` worktree. When a bug is found: reproduce → patch via PR on a branch →
ship the patch to the server as an incremental git bundle → rebuild → rerun the
simulation as regression → keep soaking.

## 2. Current state (verified 2026-08-14)

| Item | State |
|---|---|
| Server | reachable, load ~7.4 (mainnet bitcoind IBD dominates), cargo 1.97.1 via rustup |
| regtest chain | `spark-bitcoind-1` (docker, host RPC `127.0.0.1:18332`), miner loop ~1 block / 30 s, height ≈ 9386 |
| Existing nodes | `node-s/m/r` (regtest, multihop topology, healthy), `node-mut-r` (signet/mutinynet), `node-p/q` (older fix tests) |
| Existing harness | `~/multihop.sh` — S↔M↔R invoice + BOLT12 offer routing: **PASS** |
| Deployed code | `~/lampo.rs` = stale `pr551-latest`; `~/lampo-551` = stale `main@830daff`; `~/lampo-swapd-deploy` = no git (binary Aug 12) |
| Server → GitHub | **no SSH key** ⇒ code ships via git bundles over `scp` from the Mac |
| Updated main | `origin/main@e65bd89` ("lampod: reconnect channel peers automatically") — includes peer-manager fix, unified chain-sync, fundchannel fixes |

Deployed code being older than main is the core gap this plan closes:
the simulation must run **the binary built from the updated main worktree**,
not the stale swapd-deploy build.

## 3. Research summary (what the plan is based on)

### 3.1 SimLN — bitcoin-dev-project/sim-ln (payment activity generator)
The reference tool for generating realistic LN payment activity; used by LND/CLN/Eclair teams.
- Two modes: **random activity** (`--expected-payment-amount`, `--capacity-multiplier`,
  deterministic `--fix-seed`) and **defined activity**: `{source, destination,
  interval_secs, amount_msat, start_secs?, count?}`.
- Payments are executed via **keysend** so no invoice round-trip is needed.
- Environment-agnostic: "from integration tests to public signets".
- **No lampo backend** — but lampo exposes `keysend`, `pay`, `invoice`, `offer`
  over HTTP JSON-RPC, so we replicate SimLN's *activity model* directly in a
  bash harness (already proven by `multihop.sh`).
  Adopted: defined-activity schedule + seeded randomness + CSV of outcomes.

### 3.2 LND itest framework
Multi-node integration tests on regtest with explicit topology helpers,
per-test assertions on channel/payment state, and a shared miner helper.
Adopted: topology as data; hard assertions after every phase; mine-to-maturity helpers.

### 3.3 CLN test framework / lnprototest
lampo already vendors `tests/lnprototest` (BOLT conformance). That covers
protocol-level correctness; what it does *not* cover is long-running,
stateful, multi-node soak with restarts and chain events — exactly this plan's niche.

### 3.4 Mutinynet (signet)
~30 s custom signet with a public faucet (`faucet.mutinynet.com`) — already
deployed on the server (`mutiny-bitcoind-1`, `swapd-mutiny`, `node-mut-r`).
Same binary soaks on both networks in parallel: regtest gives control
(reorgs, block storms, fee spikes), mutinynet gives *real* network behaviour
(external peers, real gossip, uncontrollable block times).

### 3.5 Chaos matrix (ranked by bug-finding value for an LDK-based node)
1. **Unclean shutdown (SIGKILL) with channels open** — persistence/`manager` file integrity; a
   known past corruption mode (two writers on one `manager` file).
2. **Peer churn / restart while HTLCs in flight** — exercises the new
   auto-reconnect (`e65bd89`) and pending-HTLC recovery.
3. **Reorg of funding/commitment-confirmed blocks** (`invalidateblock` + fork) — chain-sync coordinator paths.
4. **Block storms** (10–50 blocks at once) — wallet sync cadence (2 min production schedule), confirmation racing (a known past false-failure mode).
5. **Fee spikes** (mempool spam + `estimatesmartfee`) — commitment fee handling.
6. **Channel churn** (coop close + reopen) — UTXO sweep, channel state machine.
7. **Payment mix**: bolt11 invoice / **BOLT12 offer** (needs 150 s node_announcement propagation — announcer ticks 60 s) / **keysend** (SimLN-style).
8. **Duplicate/expired invoice reuse**, over-channel-capacity payments (expected failures that must fail *cleanly*).

## 4. Simulation architecture

```
┌─────────────────────────── debian server ────────────────────────────┐
│  regtest: spark-bitcoind-1 (docker, RPC :18332) + miner loop 1blk/30s │
│                                                                       │
│   sim cluster (NEW, from ~/lampo-sim build):                          │
│     n1 ── n2 ── n3          API :8101..8106  P2P :19901..19906        │
│     │     │     │          data dirs ~/lampo-sim/nodes/nX             │
│     n4 ── n5 ── n6          (ring + chords, 2–4 hop routes)           │
│                                                                       │
│   soak driver: ~/lampo-sim/sim/simulate.sh  (endless + chaos)         │
│   artifacts:   ~/lampo-sim/artifacts/<timestamp>/                     │
│                                                                       │
│  signet: node-mut-r + swapd-mutiny (existing, parallel soak)          │
└───────────────────────────────────────────────────────────────────────┘
        ▲ git bundle (scp)          ▲ ssh
        │                           │
   Mac worktree lampo-sim-main ─────┘  (sim/main branch; PRs to main)
```

Existing `node-s/m/r` and production-ish nodes are **not touched** —
the sim cluster uses fresh data dirs and disjoint ports.

## 5. Harness design (`sim/simulate.sh`)

**Phases**
0. Preflight: bitcoind reachable, ports free, binary present.
1. Start N nodes (default 6), `wait_up` with relaunch retries (cold dir sometimes needs a second launch).
2. Fund each node, wait wallet sync (≥140 s — production sync schedule is 2 min).
3. Open channels: ring `n1..n6` + chords `n1-n3`, `n4-n6`; wait mempool → mine 8.
4. Assertions: every node ≥2 peers, every node ≥2 ready channels.
5. Wait 150 s for node_announcement propagation (BOLT12 precondition).
6. **Activity loop** (`ROUNDS`, 0 = endless): pick seeded-random (src,dst);
   method ∈ {invoice, offer, keysend}; amount log-uniform in
   [PAY_MIN_MSAT, PAY_MAX_MSAT]; assert `state=="Success"` **and** preimage
   present (never grep response text — past false-PASS bug); append to `results.csv`.
7. **Chaos event every `CHAOS_EVERY` rounds** (seeded pick):
   `restart9` (SIGKILL + cold relaunch), `storm` (mine 10–50),
   `reorg` (invalidate tip + mine 3 on fork), `feespam` (50 txs + estimatesmartfee),
   `churn` (close + reopen one channel), `zapconn` (kill TCP conns, rely on auto-reconnect).
8. Health monitor (background): `getinfo` poll every 30 s per node; log scan for
   `panic|ERROR|WARN.*corrupt`; on trip → collect artifacts → fail (or `KEEP_GOING=1`).

**Determinism**: `SEED` feeds all random choices (repayment of a failing round = same seed).
**Artifacts**: full node dirs + logs + `results.csv` tail + bitcoind mempool info, tarred per failure.

## 6. Configuration reference (env vars)

| Var | Default | Meaning |
|---|---|---|
| `BIN` | `$REPO/target/release/lampod-cli` | binary under test |
| `NODES` | 6 | cluster size (min 3) |
| `API_BASE` / `P2P_BASE` | 8100 / 19900 | port bases (node k: base+k) |
| `ROUNDS` | 10 | payments; `0` = endless |
| `SEED` | 42 | RNG seed for topology-independent choices |
| `PAY_MIN_MSAT` / `PAY_MAX_MSAT` | 10000 / 50000000 | log-uniform amount range |
| `CHAOS_EVERY` | 5 | one chaos event per N rounds |
| `METHODS` | `invoice offer keysend` | payment mix |
| `TMO` | 60 | per-RPC curl timeout (s) |
| `KEEP_GOING` | 0 | on failure: collect artifacts but continue |
| `CORE_URL`/`CORE_USER`/`CORE_PASS` | `http://127.0.0.1:18332` / testutil | bitcoind RPC |

## 7. Bug → PR → rebuild → retest loop

```
1. FAIL in simulate.sh → artifacts in ~/lampo-sim/artifacts/<ts>/
2. Mac: reproduce/analyze in worktree lampo-sim-main
3. git checkout -b fix/<slug>  … fix …  commit → push → open PR to main
4. ./sim/ship.sh fix/<slug>          # incremental git bundle → scp → server
5. server: fetch bundle, checkout branch, cargo build --release
6. rerun: ./sim/simulate.sh (same SEED) → regression green?
7. merge PR → rebase sim/main on main → ship → continue endless soak
```

`sim/ship.sh <branch>` automates 4–6 except the build itself (kicked off with
`nohup` so ssh can detach); `sim/README.md` documents the operator runbook.

## 8. Success criteria

- Endless soak (≥24 h) with 0 panics, 0 unexplained payment failures.
- Every chaos event followed by a successful multi-hop payment.
- Known-fixed regressions (peer-manager multi-inbound, fundchannel public flag,
  auto-reconnect, chain-sync) asserted every run.
- Any bug found → PR merged → simulation rebuilt on top → regression passes.

| 12:55 | **Multi-hop explicitly re-validated on the patched binary**: adapted `~/multihop.sh` to `~/lampo-mh/` (ports 8121-8123/19921-19923, fresh dirs, `~/lampo-sim` binary). PASS: invoice **S→M→R** (preimage c18b4240…) and **BOLT12 offer S→M→R** (preimage 9f392340…), path shows 2 hops with 1000msat hop fees; `M peers=2` (multi-inbound regression) and `channels ready S=1 M=2 R=1`. Note: the ring soak's payments mostly take direct channels (LDK prefers them); explicit multi-hop comes from this S↔M↔R run and from post-churn/post-#563 rerouting. |

## 9. Sources

- SimLN: https://github.com/bitcoin-dev-project/sim-ln (activity model, keysend usage)
- LND itest: https://github.com/lightningnetwork/lnd/tree/master/lnrpc + itest suite patterns
- lnprototest (already in-repo `tests/lnprototest`)
- Mutinynet: https://mutinynet.com , faucet.mutinynet.com
- Past lampo findings encoded in `~/multihop.sh` comments (peer manager, wallet-sync race,
  manager-file corruption, invoice-vs-offer propagation timing)

## 10. Run log (first day, 2026-08-14)

| Time (EDT) | Event |
|---|---|
| 08:06 | Binary built on server from `sim/main` (updated main + harness) |
| 08:08 | Smoke run #1: FAIL `n1 ready_channels=0` → artifacts collected automatically |
| 08:2x | **Bug found (real)**: `fundchannel` with malformed pubkey **panics the actix worker** (`open_channel.rs:21` `.unwrap()`) |
| 08:31 | Fix branch `fix/fundchannel-panic` shipped via git bundle, rebuilt on server |
| 08:36 | Regression verified: proper JSON-RPC error, node alive, 0 panics |
| 08:37 | **PR #561 opened** against main |
| 08:5x | Smoke #2/3: FAIL — traced via `bash -x` to harness payload typo (`"port":19902"` → 400 before handler) and a bash-arithmetic RNG that collapsed into a 3-value cycle |
| 09:48 | Harness fixed (valid JSON, tagged deterministic RNG, capped retries) |
| 10:00 | **Smoke PASS**: invoice `n3→n2` (7 s) + keysend `n2→n1` OK; chaos churn survived |
| 10:03 | Endless soak started (NODES=3, ROUNDS=0, CHAOS_EVERY=8, SEED=1337) |
| 10:36 | **Bug #2 found**: after chaos `restart9` (SIGKILL+relaunch), next `pay` from the restarted node **blocked forever** — LDK `no such monitor registered`, no failure event, handler loops on `recv()` (the in-code FIXME) → artifacts auto-collected at round 89 (88/88 payments before it) |
| 10:40 | Fix `fix/pay-event-timeout`: bound the wait with `tokio::time::timeout(120s)` → **PR #562** |
| 10:47 | Soak binary rebuilt as `sim/main` = main + both fixes (cherry-picked); regtest endless soak relaunched with `KEEP_GOING=1`; mutinet leg relaunched on the same binary (connectivity-soak mode) |
| 11:30 | Fresh cluster (SEED=4242) to keep hunting new bugs; #563 evidence preserved in `~/lampo-sim/issue-563-artifacts/`; `launch-sim.sh` cleanup pattern fixed (it was killing the mutinet harness too) |
| 11:49 | **Bug #3 reconfirmed on second seed** (restart9 probe failure, n2-involved; n2-uninvolved rounds keep passing) — not seed-specific |
| 11:22 | **Bug #3 (root cause) isolated**: same deterministic repro (SEED=1337, restart9 at round 88) — after unclean restart the node's channels are *silently broken*: payments from (89, 90), to (92) and through it stall with **no failure event**; LDK `no such monitor registered`. PR #562's timeout verified firing (4 bounded errors — graceful degradation works) → **issue #563** filed for the monitor/persistence root cause |
| 16:50 | SEED=4242 hit restart9 on n2 → probe failed; rounds 25–39 storm of 120s-timeout failures (all invoice/offer payments needing n2's channels), keysend RPC still returning (fire-and-forget — harness blind spot, needs fix) |
| 17:15 | **#563 root cause found (gdb + LDK source)**: `restart()` never calls `chain_monitor.watch_channel()` for the persisted monitors — LDK's `ChannelManagerReadArgs` doc requires moving them into the `chain::Watch` after `read()`. ChainMonitor starts empty → first update of any pre-existing channel fails `no such monitor registered` → silent breakage + 120s peer-reconnect flap. Hits **clean restarts too**, not just SIGKILL |
| 17:22 | Fix `fix/register-monitors-on-restart` committed on main, **PR #564** opened (Fixes #563); cherry-picked to sim/main (66084bf), shipped via bundle, server rebuild started |
| 17:44 | Smoke on patched binary (SEED=4242, ROUNDS=10, CHAOS_EVERY=2): **10/10 payments OK** — but the seeded picks were feespam/reorg×3/zapconn, no restart9 → manual targeted verification instead |
| 18:04 | **#563 regression VERIFIED**: manual SIGKILL n2 (2 channels open) + cold relaunch → log shows `restored channel monitor for channel … (Completed)` ×2, **0** `no such monitor registered`, peers=2 channels=2; then invoice **to** n2 (n1→n2), invoice **from** n2 (n2→n1), and BOLT12 **offer** n2→n3 all `state=Success`. Root cause closed pending PR #564 merge |
| 18:08 | Endless soak relaunched on the patched binary: NODES=3, ROUNDS=0, CHAOS_EVERY=8, SEED=5678, KEEP_GOING=1. Harness blind spot noted for v2: keysend marks Success on RPC-return only (no preimage verification) |
| 18:24 | Mutinet leg audit: was conn-soak only (faucet L402 unpayable: RouteNotFound even from node-mut-r/swapd-mutiny), nodes on the pre-#564 binary |
| 18:30 | Funded m1 on-chain from the sim bitcoind wallet (0.0005); harness: `CHANNEL_SAT` env + skip-faucet-when-funded (commits 83a7643, 0ffda9f) |
| 18:46 | **Bug #5 found**: channel open failed — `fee estimated 7092 sats` vs bitcoind's ~7 sat/vB. `fee_rate_estimation` returns **sats/kvB** but the caller uses `FeeRate::from_sat_per_vb_unchecked` → fees ×1000. Small wallets fail every open (`0.00409422 BTC needed` for a 30k sat channel); big wallets overpay ~1000× → **PR #565** `fix/fee-rate-units` |
| 19:07 | #565 shipped+rebuilt: `fee estimated 8 sats`, funding tx created, channel m1→m2 (30k sat) open → ready at 19:10; round 1 OK. #564 also verified organically: the channel **survived** the 19:15 harness-relaunch restart (`restored channel monitor` in m1's log) |
| 19:28 | Harness fixes: health() getinfo retry + broken subshell return (0ffda9f), resume-if-ready-channel (e458f3b). Reserve lesson: 0-push 30k sat channel keeps all of m2's balance under the ~1% reserve → m2 spendable 0 forever → reverse payments RouteNotFound (expected LN behaviour; **issue #566** asks for fundchannel `push_msat`) |
| 19:50 | Mutinet leg now a real payment soak: one-directional m1→m2, preimage-verified, endless. Both legs live on the patched binary (sim/main @ 818076c) |

Operational lessons encoded into the harness:
- lampo wallet applies ~1 block/s in 2-min windows → poll `wallet_height` vs `blockheight` before opening channels
- actix answers 400 (plain text) *before* the lampo handler runs — check for non-JSON bodies, don't grep for handler logs to decide if a call happened
- cold data dirs sometimes need a second launch (kept from multihop.sh)
- never `pkill -f` a pattern that appears in your own session's cmdline

