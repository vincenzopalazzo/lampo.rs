# Plan: lnd-rest-api-zeus

**Goal:** Ship a new `lampo-lnd` crate exposing an LND-compatible REST API (TLS + real macaroon bakery) so Zeus can connect to lampo as a remote "LND" node, without taking over PR #474.

**Approach:** C from `docs/brainstorms/2026-08-16-lnd-rest-api-zeus.md` — proto-derived types (`prost`/`pbjson`), handwritten Actix REST routes, shared conversion layer reusable by a later gRPC transport.

## Affected files

- `lampo-lnd/` (new) — crate: protos, auth, tls, routes, convert, server
- `Cargo.toml` — workspace member
- `lampo-common/src/conf.rs` — `lnd` API-mode switch
- `lampod-cli/src/{args,main}.rs` — select and spawn the LND REST listener
- `lampo.example.conf` — document new keys
- `tests/tests/src/` — auth + REST smoke integration
- `tools/lnd-rest-smoke.sh` — curl-based LND REST smoke (not `lncli`)
- `deny.toml` — allow new deps if needed

## Approach

1. Add `lampo-lnd` with pinned LND `v0.20.0-beta` `lightning.proto` + `routerrpc/router.proto`, compiled via `prost-build` + `pbjson-build` (vendored `protoc` when system protoc is missing).
2. Keep a transport-independent adapter that talks to `lampod::jsonrpc::*` / managers directly — **do not** route through `LampoDaemon::call` (that loops into the HTTP external handler).
3. Hand-write Actix routes matching LND grpc-gateway paths; serialize with pbjson so `uint64` is string and bytes are base64.
4. Persist stable `tls.cert`/`tls.key` and macaroon root key under the network data dir (`0700`/`0600`, no silent rotation).
5. Verify macaroons with HMAC + first-party permission caveats; authorize deny-by-default per LND entity/action table; accept `Grpc-Metadata-macaroon` hex only.
6. Milestone-1 endpoint set = Zeus P0 connect + money path (see below). History endpoints that lampo cannot back yet return empty LND-shaped lists rather than inventing data.
7. Smoke with curl against the official LND REST contract. **Official `lncli` is gRPC-only** and cannot validate a REST-only server; a later gRPC phase unlocks `lncli`.

## Milestone-1 endpoints

| Method | Path | Backing |
|--------|------|---------|
| GET | `/v1/getinfo` | inventory + managers (omit `lampo_dir`) |
| GET | `/v1/balance/blockchain` | wallet balance |
| GET | `/v1/balance/channels` | derive from channel list |
| GET | `/v1/channels` | channel manager |
| GET | `/v1/channels/pending` | pending channels (empty/partial OK) |
| GET | `/v1/channels/closed` | empty list until persisted |
| GET | `/v1/peers` | peer manager |
| POST | `/v1/peers` | connect |
| POST | `/v1/newaddress` | newaddr |
| POST | `/v1/invoices` | invoice + in-memory index |
| GET | `/v1/invoice/{r_hash}` | in-memory index + settle via events |
| GET | `/v1/invoices` | in-memory index |
| GET | `/v1/payreq/{payreq}` | decode |
| POST | `/v1/channels/transactions` | pay (unary Sync) |
| POST | `/v2/router/send` | pay as NDJSON stream (Zeus path) |
| POST | `/v1/channels` | fundchannel |
| DELETE | `/v1/channels/{txid}/{idx}` | close by funding outpoint |
| GET | `/v1/payments` | empty or recent in-memory |
| GET | `/v1/transactions` | empty or best-effort UTXO mapping |

## Edge cases / security (from review)

- `lnd=true` selects LND REST instead of legacy `lampo-httpd`; both use
  `api-host`/`api-port`, so the unauthenticated API is never exposed alongside it.
- Never log macaroons, full bodies, invoices, or preimages.
- Fail startup if LND REST bind/TLS/macaroon init fails (do not detach-and-ignore).
- Bound body/header sizes; auth before large body work.
- Stable TLS identity across restarts; no silent root-key rewrite.
- Close-by-outpoint: resolve funding txid/index against LDK channel details (Lampo close currently wants peer/channel id).
- `getinfo.version`: report `0.18.5-beta` so Zeus unlocks modern UI without claiming unsupported RPCs.

## Test plan

- Unit: macaroon verify/deny matrix; TLS restart stability; pbjson golden fixtures for GetInfo/Invoice/Channel.
- Integration (regtest via `lampo-testing`): TLS+macaroon getinfo; wrong macaroon 401; readonly cannot pay; create invoice + pay path if feasible.
- Smoke script: `tools/lnd-rest-smoke.sh` curls getinfo/balance/channels with hex macaroon (`-k`).
- Document why `lncli` is deferred (gRPC).

## Conventions

- Imperative commits, `make fmt` + targeted tests before push.
- Maintainer sign-off already implied by Approach C deps: `prost`, `pbjson*`, `rcgen`, `rustls`, `macaroon` (or minimal LND-compatible bakery if crate is unfit).
- Logs always carry `target: "lampo-lnd"`.
- No unwrap in production paths.

## Open questions / risks

- `macaroon` crate maintenance — implement thin LND bakery if crate cannot express entity/action caveats.
- pbjson vs grpc-gateway quirks — pin golden fixtures from LND v0.20 early.
- Invoice settle visibility needs event subscription; without it Zeus receive polling never sees SETTLED.
- Full Zeus E2E on a phone is manual; CI covers curl smoke + unit/integration.

## Estimated size

L (>200 LOC)

## PR sequencing inside this delivery

1. Crate + TLS + bakery + GetInfo + smoke.
2. Read endpoints (balances, channels, peers).
3. Mutating endpoints (invoice, pay, connect, open/close, newaddress).
4. Reviews (Bugbot + security) then open PR.
