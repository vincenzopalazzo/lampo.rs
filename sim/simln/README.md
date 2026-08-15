# SimLN integration (tier 4)

[SimLN](https://github.com/bitcoin-dev-project/sim-ln) generates realistic
payment activity. It supports LND/CLN/Eclair/**LDK-Server** — not lampo — but
that is enough: put **LDK-Server nodes at the edges** and **lampo nodes as the
announced relays between them**, and every simulated payment routes through
lampo.

Layout (regtest, on the debian server):

```
lk3 (ldk) ── lp1 (lampo) ── lp2 (lampo) ── lk1/lk2 (ldk, from interop tier)
   payer        relay         relay          payees
```

## Setup

1. Interop tier running (`sim/ldk-deploy.sh start 4`, `sim/interop.sh` — or a
   bigger seeded cluster from `sim/simulate.sh` plus ldk edges).
2. Channels between ldk edges and lampo relays must be **announced**
   (`--announce-channel` on the ldk side, `public:true` on the lampo side).
3. Build sim-ln on the server:

   ```bash
   git clone https://github.com/bitcoin-dev-project/sim-ln.git ~/sim-ln
   cd ~/sim-ln && make install        # needs protoc (sim/ldk-deploy.sh installs one)
   ```

4. Fill in `sim.json` from `sim.json.tpl`:
   - `api_key`: hex of `<ldk node>/data/regtest/api_key`
     (`od -An -tx1 -v api_key | tr -d ' \n'`)
   - `cert`: `<ldk node>/data/tls.crt`
   - node ids from `sim/ldk-deploy.sh status`

## Run

```bash
cd ~/lampo-main-sim/sim/simln
# random activity: ~50k sats average, 4x monthly capacity churn, deterministic
~/.cargo/bin/sim-cli --sim-file sim.json \
  --expected-payment-amount 50000sat --capacity-multiplier 4 --fix-seed 21
# or defined activity:
~/.cargo/bin/sim-cli --sim-file sim-activity.json
```

`--fix-seed` fixes the *dispatch order*; completion order still varies — treat
it as load, and keep the seeded `simulate.sh` for strict replay.

## What lampo must survive

- continuous HTLC forwarding (fee math, CLTV, failure backpropagation)
- keysend terminations from LDK edges (final CLTV delta ≥ 144)
- liquidity rebalancing pressure across relays (drain/replenish cycles)
- ldk edge churn under `sim-ln` load (combine with chaos events)

## Watching

- lampo side: `sim-run/results.csv`, `getinfo` snapshots, log health scan
- ldk side: `ldk-server-cli list-forwarded-payments` per node
- money guard: run `sim/recover.sh` STRESS against the mixed cluster.
