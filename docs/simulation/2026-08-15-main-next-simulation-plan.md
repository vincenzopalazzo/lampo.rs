# Pre-Production Simulation Plan — `sim/main-next` + LDK-Server Interop

Date: 2026-08-15
Worktree: `lampo-main-sim` (branch `sim/main-next`, tracks updated `origin/main` @ e496d21)
Target host: `vincenzopalazzo@debian`
Status: plan + first scripts landed; rollout in progress (see checklist at the end)

## 1. Goal

Production ships from `main`. Before anything reaches production, updated `main`
must soak on the debian server under the full harness (payments, chaos,
recovery). Bugs found there become PRs; the sim is rebuilt on top of the patch
and re-tested with the same seed, then the loop continues. To catch
lampo-specific (vs LDK-generic) defects, a **second implementation** —
[LDK-Server](https://github.com/lightningdevkit/ldk-server) — joins the same
regtest network so every channel, invoice, route and reconnect is exercised
cross-implementation. Finally, [SimLN](https://github.com/bitcoin-dev-project/sim-ln)
drives realistic payment activity through the cluster.

## 2. Deep research summary (sources)

### 2.1 LDK-Server (lightningdevkit/ldk-server)
- Daemon built on **LDK Node** (LDK + BDK wallet), API-first **gRPC over TLS**
  (default `127.0.0.1:3536`, self-signed ECDSA P-256 cert auto-generated at
  `<storage_dir>/tls.crt`, clients must pin it).
- Auth: every request carries `x-auth: HMAC <ts>:<hmac_hex>` where
  `hmac = HMAC-SHA256(api_key, ts_be_8b || grpc_body)`; ±60 s clock window.
  The API key is written to `<storage>/<network>/api_key` (raw bytes).
- Config (`contrib/ldk-server-config.toml`): `network = "regtest"`,
  `[node] listening_addresses`, `grpc_service_address`, `alias`,
  `announcement_addresses`; **native `[bitcoind]` RPC backend** (no esplora
  needed — perfect for our `spark-bitcoind-1` at `127.0.0.1:18332`);
  `[storage.disk] dir_path`; `[log] log_to_file/level`; optional `[metrics]`.
- CLI driver: `ldk-server-cli --base-url :PORT --api-key <hex> --tls-cert ...`
  with subcommands mirroring the API: `get-node-info`, `connect-peer`,
  `open-channel` (**`--announce-channel`** — channels are unannounced by
  default!), `close-channel`, `bolt11-receive`, `bolt11-send`,
  `bolt12-receive/send`, `spontaneous-send`, `onchain-receive`,
  `get-balances`, `list-peers`, `list-channels`, `splice-channel`.
- **Min final CLTV expiry delta 144** on inbound keysend (senders must comply).
- Workspace also ships an **MCP bridge** (`ldk-server-mcp`) — interesting later
  for letting an agent drive a node as tools.
- Pre-1.0 (data model may break) — irrelevant for a disposable sim cluster.

### 2.2 SimLN (bitcoin-dev-project/sim-ln)
- Reference LN **activity simulator** (LND/CLN/Eclair/**LDK-Server** backends;
  lampo is not supported — see workaround below).
- Node entry for LDK-Server:
  `{"address": "ip:port", "api_key": "<hex of api_key file>", "cert": "<tls.crt>"}`
  (no `id` needed; fetched automatically).
- Two modes: **random activity** (topology-driven; flags
  `--expected-payment-amount`, `--capacity-multiplier` — monthly multiples of
  channel capacity, `--fix-seed <u64>` for deterministic dispatch order) and
  **defined activity** (`source`, `destination`, `interval_secs`, `amount`,
  fixed or distribution).
- Payments use **keysend**; LDK-Server accepts them by default
  (`spontaneous_send`).
- **Workaround for lampo**: sim-ln only needs to *drive* the edge nodes. If
  LDK-Server nodes sit at the edges and lampo nodes are the announced relays
  between them, every simulated payment routes **through** lampo — exercising
  its forwarding, fee math, gossip and failure handling under realistic load
  without native sim-ln support.

### 2.3 Multinet (tonelabs/multinet)
- Repository is gone (404 on GitHub, incl. search). Its role (multi-node,
  multi-impl LN testnets from config) is covered natively by our harness +
  docker regtest backend + LDK-Server nodes. The existing `mutinet.sh` tier
  (mutinynet **signet**, docker `mutiny-bitcoind-1`) keeps covering the
  public-network flavor.

## 3. Architecture — tiered simulation

| Tier | What | Where | Ports (API / P2P) | State |
|---|---|---|---|---|
| 1 | lampo-only regtest soak + multihop + recovery matrix, `sim/test-573-577` (main + 9 unmerged fixes) | `~/lampo-sim` | 8101+ / 19901+, mh 8211-13 / 20111-13 | running (unchanged) |
| 2 | **main-next soak** — same harness against updated `origin/main` | `~/lampo-main-sim` | 8301+ / 20101+ | NEW |
| 3 | **interop** — lampo ↔ LDK-Server mixed cluster | `~/lampo-main-sim` | lampo 8321-22 / 20121-22; ldk gRPC 3541-42, P2P 9841-42 | NEW |
| 4 | **SimLN realistic activity** through lampo relays | same cluster | ldk edges 3543+ / 9843+ | planned |
| 5 | mutinynet signet cluster | `~/lampo-sim` m1/m2 signet | — | running (unchanged) |

All regtest tiers share the bitcoind at `127.0.0.1:18332`
(`testutil` / `testutilpassword`, wallet `default`). Port families are
disjoint so every tier runs in parallel. Production nodes (`node-s/m/r`,
mainnet node) are never touched.

Tier-3 topology (dead-end by construction — lp1 and lp2 share no channel):

```
lp1 (lampo) ──c1── lk1 (ldk-server) ──c2── lk2 (ldk-server) ──c3── lp2 (lampo)
```

- c1: opened by **lampo** (`fundchannel` `public:true`, push_msat so lk1 has
  outbound liquidity)
- c2, c3: opened by **ldk-server** (`open-channel --announce-channel`)

Every lp1↔lp2 payment structurally crosses both implementations twice.

## 4. Interop assertions (sim/interop.sh)

| ID | Case | Pass condition |
|---|---|---|
| I01 | lp1 ↔ lk1 connect (lampo initiates) | peer visible on both sides |
| I02 | c1 open lampo→ldk | ready on both, ≥6 conf |
| I03 | c2 open ldk→ldk (announced) | ready on both |
| I04 | c3 open ldk→lampo (announced) | ready on both |
| I05 | bolt11 lampo→ldk (lp1 pays lk1 invoice) | Success + preimage |
| I06 | bolt11 ldk→lampo (lk1 pays lp1 invoice) | Success + preimage |
| I07 | keysend lampo→ldk (spontaneous inbound, CLTV ≥144) | Success + preimage |
| I08 | spontaneous-send ldk→lampo | Success + preimage |
| I09 | **cross-impl multihop** lp1→lp2 invoice | Success + preimage + hops ≥3 + both ldk ids on path |
| I10 | reverse: lp2→lp1 invoice | same |
| I11 | bolt12: lp1 pays lp2 offer (after gossip wait) | Success + preimage |
| I12 | chaos: SIGKILL lk1 → restart → auto-reconnect | peer back, no manual action |
| I13 | post-chaos payment lp1→lp2 | Success + preimage |
| I14 | health: lampo log delta | no `panic|corrupt|invariant` |

Rows append to `$SIMDIR/interop-results.csv`; failures collect artifacts
(node dirs, logs, getinfo, mempool) exactly like `simulate.sh`.

## 5. Configuration matrix

Tier 2 (main-next soak) — same knobs as the existing soak:

| Var | Default | Meaning |
|---|---|---|
| `NODES` | 6 | cluster size (n1..nN) |
| `ROUNDS` | 10 | 0 = endless soak |
| `SEED` | 42 | deterministic RNG, replayable |
| `PAY_MIN_MSAT` / `PAY_MAX_MSAT` | 10000 / 50000000 | log-uniform amounts |
| `CHAOS_EVERY` | 5 | chaos events per N rounds |
| `METHODS` | invoice offer keysend | payment methods |
| `KEEP_GOING` | 0 | 1 = log failures, keep soaking |
| `REPO`/`BIN`/`SIMDIR` | `~/lampo-main-sim`… | separate clone/data-dir |

Tier 3 (interop):

| Var | Default | Meaning |
|---|---|---|
| `LDK_REPO` | `~/ldk-server` | ldk-server clone on server |
| `LDK_REF` | `main` | pin a tag for reproducibility |
| `LDKDIR` | `$REPO/ldk-nodes` | lk1/lk2 data dirs + configs |
| `LDK_GRPC_BASE` | 3540 | gRPC = base+idx |
| `LDK_P2P_BASE` | 9840 | P2P = base+idx |
| `CHANNEL_SAT` | 1000000 | channel size for c1..c3 |

Tier 4 (SimLN, `sim/simln/`): random activity with
`--expected-payment-amount 50000sat --capacity-multiplier 4 --fix-seed 21`
plus a defined-activity file for hot paths (lp-payee offers). Edge nodes are
LDK-Server; lampo relays carry the traffic. Metrics from `sim-cli` CSV output
+ lampo `results.csv`.

## 6. Bug → PR → rebuild → retest loop (updated)

1. Failure ⇒ artifacts in `<clone>/sim-run/artifacts/<ts>-*/` → `scp` to Mac.
2. In worktree `lampo-main-sim`: `git checkout -b fix/<slug> origin/main`,
   fix, commit (DCO), push, open PR.
3. `LAMPO_REMOTE_DIR=~/lampo-main-sim ./sim/ship.sh fix/<slug>` — bundle →
   fetch → checkout → `cargo build --release` (`BUILD_OK` in `build.log`).
4. Regression with the **same SEED**; then also `./sim/interop.sh` (a fix must
   not break cross-impl behavior), then resume endless soak.
5. PR merged ⇒ `git fetch origin && git checkout sim/main-next &&
   git rebase origin/main` ⇒ reship ⇒ keep soaking.
6. The 9 fixes currently ahead on `sim/test-573-577`
   (`funding-locktime`, `external-handler-rwlock-starvation`,
   `nonfinal-broadcast-retry`, `restart-monitor-registration`, …) graduate to
   PRs against main one by one; main-next picks them up via rebase.

## 7. Rollout checklist (this deployment)

- [x] New worktree `lampo-main-sim` on updated `origin/main` (e496d21)
- [x] Harness version-controlled inside the repo (`sim/`, pulled from server)
- [x] Research: ldk-server, sim-ln, multinet (documented above)
- [x] `sim/ship.sh` parameterized (`LAMPO_REMOTE_DIR`)
- [x] `sim/ldk-deploy.sh` (clone/build/protoc/config/run ldk-server nodes)
- [x] `sim/interop.sh` (tier-3 mixed cluster + assertions I01–I14)
- [x] `sim/simln/` templates (tier 4)
- [x] Ship `sim/main-next` → `~/lampo-main-sim`, release build `BUILD_OK`
- [x] `ldk-deploy.sh` on server: protoc + ldk-server + cli built, lk1/lk2 up
- [x] `interop.sh` first run: found harness scope bug (`bash -c` hid lib.sh
      functions → checks now `eval` in the harness shell)
- [ ] `interop.sh` full pass on server — **I01 green** (lampo↔ldk peering OK);
      **I02 blocked by interop finding F-3** (see below)
- [x] **F-3 CLOSED (environment, not lampo)**: channel to an LDK-Server peer
      took ~8.5 min to flip ready because the shared regtest bitcoind's
      rpcworkqueue (503s) starved the ldk confirmation polling. Fixed ops-side:
      `rpcthreads=16 rpcworkqueue=256` in `~/spark-stack/docker/bitcoin.conf`;
      payment over the channel verified Success once ready. Related incident
      recovery: the bitcoind restart corrupted/unloaded the `default` wallet —
      quarantined, salvaged via `bitcoin-wallet dump|createfromdump`
      (`default-recovered`, full-chain rescan ~25 min), drained 14.4k BTC back
      into a fresh `default` (regtest subsidy is 0 at height 20k+, mining
      cannot re-fund). NOTE: createwallet 2nd positional arg is
      disable_private_keys — load_on_startup is a named arg.
- [x] **F-4 FOUND & FIXED (lampo, PR #588)**: `FeeEstimator` stub returned a
      hardcoded 256 for ALL ConfirmationTargets — including the
      `MinAllowed*RemoteFee` acceptance floors, which must be 253 s/kW — so
      lampo rejected channel opens from any standard-floored funder
      ("Peer's feerate much too low. Actual: 253. lower limit: 256").
      Fix returns 253 for the two MinAllowed targets; **verified live**:
      I04 (ldk opens to lampo) passes on the patched build.
- [x] **F-5 RESOLVED — two stacked causes**:
      1. F-4's fee-floor bug prevented announced/usable channels (fixed by
         PR #588), and
      2. lampo's `fundchannel` silently dropped `push_msat` (hardcoded 0) so
         the ldk peer had `outbound_capacity_msat: 0` — **PR #569** fixes it
         and was verified live (lk1 outbound 91.2M msat after push).
      Plus a harness fix: ldk-server `bolt11-send`/`spontaneous-send` are
      ASYNC (return `payment_id`) — poll `get-payment-details` for Success.
- [x] **Interop suite I01–I09 GREEN** on `sim/interop-verify`
      (main + #588 + #569): connect, channels opened by BOTH sides
      (announced), bolt11 both directions, keysend/spontaneous both
      directions, and **cross-impl multihop lp1→lp2 with hops=3 via both
      ldk nodes (lampo→ldk→ldk→lampo routing works)**.
- [x] **F-7 CLOSED (harness topology, not a lampo bug)**: reverse multihop
      failed because lk2 had no outbound on c2 (no push) and stale duplicate
      channels from verification reruns poisoned routing. Fix: push on BOTH
      relay channels + fresh ldk slate between runs.
- [x] **FULL INTEROP PASS: I01–I14, 14 OK / 0 FAIL** on `sim/interop-verify`
      (main + #588 + #569): connect; channels opened by both sides
      (announced); bolt11 BOTH directions; keysend/spontaneous BOTH
      directions; cross-impl multihop BOTH directions (hops=3, via both ldk
      nodes); **bolt12 offer across implementations**; **ldk SIGKILL+restart
      chaos with a payment succeeding through the recovered mixed cluster**;
      lampo logs clean. The alternative-implementation tier is fully green.
- [ ] Old: F-7 reverse multihop I10 fails —
      `state=Failure` with an empty path. Note: the verification reruns
      created duplicate parallel channels (one per run) on the persistent
      ldk nodes; dedup or a fresh ldk slate first, then check lp2's route
      view / PaymentEvent `reason` (cross_pay drops it — log it).
      Artifacts: `interop/artifacts/20260815-213852-I10`.
- [ ] Old: F-5 ldk-server paying a lampo bolt11 invoice fails
      ("Failed to send the given payment", case I06, direct hop over c1).
      Artifacts: `interop/artifacts/20260815-205851-I06`. Suspects: invoice
      routing hints/features from lampo, outbound liquidity view on the ldk
      side of c1, CLTV/feature mismatch.
- [x] Interop suite on the patched build: **I01..I05 green** (connect, c1
      lampo->ldk, c2 ldk->ldk announced, c3 ldk->lampo announced, bolt11
      payment lampo->ldk 1.25M msat Success) — first full cross-impl
      topology + payment in lampo history.
- [ ] Old text: F-3 (open, lampo side): channel to LDK-Server peer never flips
      `ready:true`** — funding confirmed (short_channel_id assigned, balances
      populated), lk1 received `channel_ready`, but lampo reports
      `ready:false` and payments fail with `RouteNotFound` (verified manually:
      lp1→lk1 bolt11 over c1). Suspects: announcement_signatures flow for
      public channels; ready-derivation in the channels handler. Artifacts:
      `interop/artifacts/20260815-164023-*`. Harness bugs fixed along the way:
      bash -c scope (eval), lib.sh P2P() lampo-names-only (inline fundchannel),
      ldk bolt11-receive positional amount.
- [x] Tier-2 smoke (3 nodes / 2 rounds) — **found 2 main bugs**:
      1. `/keysend` REST route never registered (empty-body 404) →
         **PR #585** (`fix/httpd-keysend-route`, shipped+rebuilt, route and
         handler verified live)
      2. channel monitors not re-registered on restart → payments hang
         forever (`Failed to update channel monitor: no such monitor
         registered`) → **PR #580** (`fix/restart-monitor-registration`,
         cherry-pick of 8a68eb3)
- [x] smoke3 regression on fresh data dirs (same SEED=42) validates #585
      (keysend Success+preimage 15s) — **#585 merged into main** (1ce4261);
      #568 keeps only the 120s-bounded-wait value (noted on the PR)
- [x] PR #564 verified with the restart regression (SIGKILL+relaunch →
      invoice Success+preimage 4.8s, zero monitor errors); #580 closed as
      byte-identical duplicate
- [x] Endless soak **running**: `sim/soak-combined` (main+#585+monitor fix),
      NODES=6 ROUNDS=0 SEED=1337 KEEP_GOING=1 — 67+ OK rounds, chaos firing
      (storm/reorg/feespam/churn/zapconn/restart9); round-5 keysend failure
      artifacted for triage (`artifacts/20260815-155804-*`)
- [ ] Endless soak on main-next once both PRs merge
- [ ] Tier-4 sim-ln wiring
- [ ] Remaining fixes from `sim/test-573-577` graduated to PRs

## 8. Risks / notes

- **protoc** missing on server — `ldk-deploy.sh` installs via apt (passwordless
  sudo) or a user-local release zip with `PROTOC` env.
- Parallel cargo builds on 8 cores / ~10 GB free RAM — build lampo and
  ldk-server sequentially, or cap `CARGO_BUILD_JOBS`.
- ldk-server keysend needs sender final CLTV delta ≥ 144 — if lampo's default
  is lower, that's a genuine interop finding to fix in lampo (route params).
- ldk-server CLI flag spellings can drift (pre-1.0): helpers log raw responses;
  `LDK_DEBUG=1` prints `--help` for each subcommand used.
- Never reuse data dirs or ports across tiers; never touch production nodes.
