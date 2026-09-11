# SimLN integration (optional)

[SimLN](https://github.com/bitcoin-dev-project/sim-ln) generates realistic
payment activity. It speaks LND/CLN/Eclair/**LDK-Server** RPCs — **not lampo**.

That means SimLN **cannot** prove lampo send/receive by itself. Use it as an
optional **forwarding load** generator on top of the native soak.

## What each layer proves

| Layer | Lampo as sender | Lampo as receiver | Lampo as relay |
|-------|-----------------|-------------------|----------------|
| `simulate.sh` edge-role matrix + soak | **required gate** | **required gate** | exercised when path ≥2 hops |
| `multihop.sh` | yes (hs/hr) | yes (hs/hr) | yes (hm) |
| `interop.sh` | yes ↔ LDK | yes ↔ LDK | cross-impl path |
| SimLN (this dir) | no (LDK pays) | no (LDK invoiced) | **yes** (lampo mid-path) |

**Phase 2 soak-green always means the native harness gates**, not SimLN.

## Relay-load topology (SimLN)

Put LDK-Server at the edges SimLN can drive, and lampo as announced mid-hops:

```
lk3 (ldk) ── lp1 (lampo) ── lp2 (lampo) ── lk1/lk2 (ldk)
   payer        relay         relay          payees
```

Useful for graph/HTLC forwarding under synthetic traffic. It does **not**
replace lampo-as-edge coverage in `simulate.sh`.

## Mixed-edge goal (follow-up)

Prefer clusters where **lampo is also an edge** for some flows:

- native harness: lampo → lampo and lampo ↔ LDK (`interop.sh`)
- SimLN: LDK → … → LDK through lampo relays (this directory)

Until SimLN grows a lampo adapter, do not treat “SimLN green” as send/recv proof.

## Setup

1. Interop tier running (`./simulations/ldk-deploy.sh start 4`, `./simulations/interop.sh` —
   or a bigger seeded cluster from `./simulations/simulate.sh` plus LDK edges).
2. Channels between LDK edges and lampo relays must be **announced**.
3. Build sim-ln:

   ```bash
   git clone https://github.com/bitcoin-dev-project/sim-ln.git ~/sim-ln
   cd ~/sim-ln && make install
   ```

4. Copy `sim.json.tpl` → `sim.json` and fill:
   - `api_key`: hex of `<ldk node>/data/regtest/api_key`
   - `cert`: path under `$LDK_HOME` (see template placeholders)
   - node ids from `./simulations/ldk-deploy.sh status`

## Run

```bash
cd simulations/simln
# random activity
sim-cli --sim-file sim.json \
  --expected-payment-amount 50000sat --capacity-multiplier 4 --fix-seed 21
# or defined activity:
sim-cli --sim-file sim-activity.json
```

`--fix-seed` fixes dispatch order; completion order still varies — treat it as
load, and keep seeded `simulate.sh` for strict replay / send-recv gates.

## Watching

- lampo: `$SIMDIR/results.csv`, `getinfo` snapshots, log health
- ldk: `ldk-server-cli list-forwarded-payments` per node
- money guard: `./simulations/recover.sh` STRESS against the mixed cluster
