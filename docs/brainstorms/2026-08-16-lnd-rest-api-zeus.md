# LND-compatible REST API for lampo, validated with Zeus

Date: 2026-08-16
Status: brainstorm (decisions locked with maintainer, see below)

## Context

- Zeus (https://github.com/ZeusLN/zeus) remote backends: **LND REST**
  (macaroon auth, hex, sent as `Grpc-Metadata-macaroon` header), CLN
  clnrest (rune auth), LNC, LNDHub, and Nostr Wallet Connect. Zeus does
  **not** speak LND gRPC to remote nodes.
- PR #474 (https://github.com/vincenzopalazzo/lampo.rs/pull/474) adds a
  `lampo-grpc` tonic crate with the full LND proto dump but only
  `GetInfo` and `ConnectPeer` implemented. Stale since Sept 2025;
  predates the async migration. Decision: **do not take it over** —
  start fresh, reference it for ideas only.
- Roadmap issue #392 already lists "Implementing an LND-compatible API
  like rld and making it the primary one" plus "dropping the generic
  `call` in favour of strongly typed calls like `node.info()`".
- Current `lampo-httpd` (actix-web, paperclip, no auth) exposes:
  getinfo, funds, networkchannels, decode, invoice, keysend, pay,
  offer, newaddr, channels, close, connect, fundchannel, stop. Most
  core operations exist; the work is wire-format compatibility plus
  authentication.

## Decisions (from maintainer, 2026-08-16)

1. **Goal**: LND API compatibility is the strategic deliverable; Zeus
   is the first real client used to validate it.
2. **PR #474**: start a fresh crate; reference the PR only.
3. **Surface**: LND REST first; gRPC later, sharing a typed layer.
4. **Auth**: full LND-style macaroon bakery (permissions/caveats) plus
   TLS from day one.

## Clarified Problem Statement

**Goal:** Ship a new crate exposing an LND-compatible REST API (with
macaroon + TLS auth) sufficient for Zeus to connect to lampo as a
remote "LND" node.

**Constraints:**
- Wire fidelity with LND's REST conventions (grpc-gateway style JSON:
  camelCase/snake_case exactly as lnd emits, bytes as base64, uint64
  as string) — Zeus parses these shapes strictly.
- Macaroon auth compatible with what Zeus sends (`Grpc-Metadata-
  macaroon` header, hex-encoded), backed by a real bakery with
  permission caveats; TLS with self-signed cert generated at startup
  (lnd-style `tls.cert`/`tls.key`, `admin.macaroon` on disk).
- Must not regress the existing JSON-RPC / lampo-httpd surfaces.
- New dependencies need maintainer sign-off (repo rule). Expected:
  `prost`/`pbjson` or hand-rolled serde types, `macaroon` crate,
  `rcgen` for cert generation.
- Each commit self-contained, passes `make fmt` + `make check`.

**Non-goals:**
- gRPC server itself (later phase; design must not block it).
- LNC, LNDHub, NWC, or clnrest backends.
- Streaming/subscription endpoints (`SubscribeInvoices`, channel event
  streams) beyond what Zeus strictly needs for the first milestone.
- Full lnrpc coverage — only the Zeus-critical subset (below).
- Taking over or rebasing PR #474.

**Success criteria:**
- Zeus (Android/iOS build or desktop) connects to lampo on regtest/
  signet using the "LND (REST)" node interface with host, port, and
  hex macaroon, over TLS.
- Zeus can: view balances and channels, generate an invoice, receive
  a payment, pay an invoice, decode a payreq, get a new on-chain
  address, connect a peer, open and close a channel.
- Rejected/absent/wrong-permission macaroons are refused with
  LND-shaped errors.
- Integration test in `tests/` exercising the REST surface (curl-level
  fidelity, e.g. against fixtures captured from a real lnd).

**Zeus-critical endpoint subset (first milestone):**
`GET /v1/getinfo`, `GET /v1/balance/blockchain`,
`GET /v1/balance/channels`, `GET /v1/channels` (+ pending, closed),
`GET/POST /v1/invoices`, `GET /v1/payreq/{payreq}`,
`POST /v2/router/send` (or `/v1/channels/transactions`),
`GET /v1/payments`, `GET /v1/transactions`, `POST /v1/newaddress`,
`GET /v1/peers`, `POST /v1/peers` (connect), `POST /v1/channels`
(open), `DELETE /v1/channels/{funding_txid}/{output_index}` (close).

## Approaches Considered

### Approach A: Hand-written REST crate (mirror lampo-httpd pattern)
- Sketch: new `lampo-lnd` crate with actix-web routes; hand-written
  serde structs for each LND request/response; each handler calls the
  existing `LampoDaemon` handler like `lampo-httpd` does today.
- Affected files: new `lampo-lnd/`, `lampod-cli/src/main.rs` +
  `args.rs` (spawn the server), `Cargo.toml` workspace.
- Tradeoffs: fastest to first Zeus connection; no proto toolchain.
  But every type is transcribed by hand (drift risk vs. lnd), and a
  future gRPC server shares nothing — the typed layer would be
  rebuilt.
- Effort: M

### Approach B: Typed-core-first (roadmap-pure)
- Sketch: first land the strongly-typed internal API from #392
  (`node.info()`, `node.pay()`, ... on `LampoDaemon`), refactor
  lampo-httpd/jsonrpc onto it, then add the LND REST crate as a thin
  adapter over the typed core.
- Affected files: `lampod/src/` broadly (handler, jsonrpc), then new
  `lampo-lnd/`.
- Tradeoffs: best long-term architecture and directly executes the
  roadmap experiment; gRPC later is trivial. But the refactor is a
  large, churny prerequisite that delays Zeus validation by weeks and
  touches everything.
- Effort: L (XL including the refactor)

### Approach C: Proto-derived types, REST-first (recommended)
- Sketch: new `lampo-lnd` crate vendoring `lightning.proto` (+
  `router.proto`), compiled with `prost` + `pbjson` so the generated
  Rust types serialize to exactly the JSON grpc-gateway emits.
  Hand-write the REST routes (actix-web, matching lnd's URL mapping),
  each converting proto types <-> lampo model and calling the daemon.
  A later gRPC phase reuses the identical generated types and
  conversion layer, adding only a tonic transport.
- Affected files: new `lampo-lnd/` (build.rs, protos, `routes/`,
  `convert/`, `auth/`), `lampod-cli` wiring, workspace `Cargo.toml`,
  `deny.toml` for new deps.
- Tradeoffs: guaranteed wire fidelity and a shared typed layer for
  free (the proto types *are* the contract), directly reusable for
  gRPC. Costs: proto toolchain in the build (protoc via
  `protoc-bin-vendored` or committed generated code), and pbjson's
  proto3-JSON mapping must be spot-checked against grpc-gateway
  output for a few endpoints.
- Effort: L

## Recommendation

Approach C. It satisfies both stated goals with one artifact: the
prost/pbjson-generated types give LND wire fidelity for Zeus today and
become the shared typed layer the future gRPC server needs, without
first paying for the Approach B core refactor. I'd sequence it as:
(1) crate skeleton + TLS + macaroon bakery + `/v1/getinfo`;
(2) read-only endpoints (balances, channels, invoices, payments,
decode); (3) mutating endpoints (invoice create, pay, newaddress,
connect, open/close); (4) Zeus end-to-end validation on signet.
One honest uncertainty: pbjson matches canonical proto3 JSON, while
grpc-gateway (lnd) has a couple of quirks (e.g. some snake_case
fields, enums as strings) — verify with captured lnd responses early,
in step (1), before mass-producing endpoints.

## Open questions

- Macaroon crate choice: `macaroon` (rust-macaroon) is barely
  maintained — vendor, fork, or accept it? Needs maintainer sign-off
  per dependency rule.
- Does Zeus require any streaming endpoint for its core screens (e.g.
  invoice subscription for the receive screen), or does it poll on
  REST? Verify against the Zeus LND backend source before scoping
  milestone 1.
- Where does the LND listener config live — new `[lnd]` section in
  `lampo.conf` (port, tls dir, macaroon dir)?
- Should `lampo-httpd` eventually be deprecated in favour of this
  surface ("making it the primary one" per #392)? Out of scope now,
  worth a roadmap note.
- Version string in `getinfo`: Zeus gates features on lnd versions
  (e.g. `v0.20.0` checks). Decide what lampo reports (mimic an lnd
  version vs. honest lampo version — affects Zeus feature gating).
