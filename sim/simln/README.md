# SimLN integration (optional)

[SimLN](https://github.com/bitcoin-dev-project/sim-ln) generates realistic
payment activity. It supports LND/CLN/Eclair/**LDK-Server** — not lampo — but
that is enough: put **LDK-Server nodes at the edges** and **lampo nodes as the
announced relays between them**, and every simulated payment routes through
lampo.

```
lk3 (ldk) ── lp1 (lampo) ── lp2 (lampo) ── lk1/lk2 (ldk)
   payer        relay         relay          payees
```

## Setup

1. Interop tier running (`./sim/ldk-deploy.sh start 4`, `./sim/interop.sh` —
   or a bigger seeded cluster from `./sim/simulate.sh` plus LDK edges).
2. Channels between LDK edges and lampo relays must be **announced**.
3. Build sim-ln:

   ```bash
   git clone https://github.com/bitcoin-dev-project/sim-ln.git ~/sim-ln
   cd ~/sim-ln && make install
   ```

4. Copy `sim.json.tpl` → `sim.json` and fill:
   - `api_key`: hex of `<ldk node>/data/regtest/api_key`
   - `cert`: path under `$LDK_HOME` (see template placeholders)
   - node ids from `./sim/ldk-deploy.sh status`

## Run

```bash
cd sim/simln
# random activity
sim-cli --sim-file sim.json \
  --expected-payment-amount 50000sat --capacity-multiplier 4 --fix-seed 21
# or defined activity:
sim-cli --sim-file sim-activity.json
```

`--fix-seed` fixes dispatch order; completion order still varies — treat it as
load, and keep seeded `simulate.sh` for strict replay.

## Watching

- lampo: `$SIMDIR/results.csv`, `getinfo` snapshots, log health
- ldk: `ldk-server-cli list-forwarded-payments` per node
- money guard: `./sim/recover.sh` STRESS against the mixed cluster
