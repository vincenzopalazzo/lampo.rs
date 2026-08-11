# swapd stress deployment (regtest on a server)

A self-contained deployment of the swap daemon plus a self-hosted Spark
operator stack, for hammering the swap logic under fault injection on
real hardware with worthless coins. Regtest is chosen over a public
signet on purpose: it lets the chaos driver *force* reorgs, withhold
blocks, and stall operators — the conditions that actually stress the
timelock and recovery paths — which a public chain will not.

## Why not literal mutinynet

Spark's SDK hosts operators for **mainnet only**
(`default_operator_pool_config` returns the same three mainnet operators
for every network). Signet/mutinynet therefore requires *self-hosted*
operators, and the `buildonspark/spark` operator config ships pinned to
`network: regtest`. Running it on signet is unverified operator-side
work (a signet config block, faucet funding, 30s cadence). For a
crash-test, self-hosted regtest is both proven and strictly more
controllable, so it is the deployment here; public mutinynet is a
fidelity follow-on.

## Layout on the server (vincenzopalazzo@debian, amd64)

- `~/spark-stack/` — the operator stack (`docker compose`, project name
  **`spark`** so containers are `spark-bitcoind-1`,
  `spark-spark-operator-{0,1,2}-1`, matching the e2e test constants).
  - bitcoind host port remapped `8332 -> 18332` to avoid the existing
    host bitcoind. Operators reach bitcoind internally by service name.
  - operator image `spark-operator:local`, **rebuilt natively for
    amd64** on the server (the dev laptop's image is arm64).
- `/tmp/spark-tls/ca.crt` — the CA the operators' leaf certs are signed
  by; the daemon and tests trust it (rustls rejects a self-signed cert
  used as both CA and leaf).
- `~/lampo-swapd-deploy/` — the source at the deployed commit; the
  release binary is `lampo-spark-swapd/target/release/lampo-spark-swapd`.
- Toolchain: rustup (`~/.cargo`), and a no-sudo `protoc` in
  `~/.local/bin` (export `PROTOC=$HOME/.local/bin/protoc`).

## Bring-up

```bash
# 1. operator stack (project name must be `spark`)
cd ~/spark-stack && docker compose -p spark up -d
# wait until the three operators are healthy and the deposit init funded

# 2. the CA where the daemon/tests expect it
mkdir -p /tmp/spark-tls && cp ~/spark-stack/tls-local/*.crt /tmp/spark-tls/

# 3. build (if not already)
source ~/.cargo/env && export PROTOC=$HOME/.local/bin/protoc
cd ~/lampo-swapd-deploy/lampo-spark-swapd && cargo build --release
```

## Stress run

```bash
cd ~/lampo-swapd-deploy/lampo-spark-swapd
deploy/stress.sh 25          # 25 rounds, a fault injected between each
```

Each round runs the real swap e2e suite (both directions + crash
recovery) against the live operators; between rounds the driver restarts
an operator, reorgs bitcoind, or stalls a container. It stops on the
first failing round and dumps the log — that is the crash. A clean run
reports the rounds survived.

## Config knobs that bound risk (set in the daemon's lampo.conf)

- `swap-max-sat` — largest accepted swap; keep small.
- `swap-fee-base-sat` / `swap-fee-ppm` — the spread; Direction A also
  floors at LDK's routing budget so a swap never settles at a loss.
- `spark-operator=<id>|https://127.0.0.1:853{5,6,7}|<identity>|/tmp/spark-tls/ca.crt`
