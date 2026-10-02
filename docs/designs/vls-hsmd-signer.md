# VLS behind the signer seam: an hsmd wire client for lampo

| Field | Value |
|-------|-------|
| Status | Draft |
| Author | Vincenzo Palazzo (with Claude-assisted research) |
| Date | 2026-09-30 |
| Builds on | `LampoSigner` seam (PR #628), `docs/brainstorms/2026-09-30-vls-modular-signer.md` |
| Sources | VLS `v1.0.0-rc.1` / `main` (`vls-protocol`, `vls-protocol-client`, `vls-proxy`, `vlsd`, `vls-frontend`), lnrod `main` (2025-02-26) |

## Summary

Lampo's signer is now an `Arc<dyn LampoSigner>`. This document specifies the
second implementation: a `lampo-vls` crate that keeps node and channel keys in
the Validating Lightning Signer (`vlsd`) and talks to it through the CLN hsmd
wire protocol. It records the wire mapping for every `LampoSigner` and
`LampoChannelSigner` method, the channel setup sequence, what the node must
persist, what the signer needs from the chain, and the policy implications for
lampo's wallet-owned scripts.

The mapping was read out of `vls-protocol-client/src/lib.rs`, which is VLS's own
LDK adapter. That adapter cannot be linked into lampo (it implements the
`lightning 0.2` traits), but it is the reference for what a lampo-owned client
must send. LDK 0.2.4 and 0.3 have identical method sets on `EntropySource`,
`NodeSigner`, `SignerProvider`, `OutputSpender`, `ChannelSigner`,
`EcdsaChannelSigner` and `ChangeDestinationSourceSync`, so the mapping carries
over one to one.

## Why a wire client and not a dependency

- `vls-core` pins `lightning = "0.2.4"`; lampo is on `0.3.0-rc2`. Every VLS crate
  that implements LDK traits (`vls-protocol-client`, `vls-proxy`,
  `vls-frontend`) implements them for the wrong `lightning`, so their signer
  types cannot be handed to lampo's `ChannelManager`.
- `vls-protocol` (the message definitions) also depends on `vls-core` with
  default features off, so it pulls `lightning 0.2.x` as a *second* version.
  Cargo resolves that (`lightning v0.2.6` and `v0.3.0-rc2` side by side,
  `bitcoin 0.32` unified), and no `lightning` type crosses the boundary:
  message fields are `bitcoin` and `serde_bolt` types. But CI's
  "single LDK version" guard (the #537 failure mode) rightly rejects a
  second `lightning` in lampo's build. So `lampo-vls` lives behind a `vls`
  cargo feature: it is a workspace member but not a default member, an
  optional dependency of `lampod-cli` and `lampo-testing`, and the guard
  runs with `--exclude lampo-vls`. The default build never contains
  `lightning 0.2`.
- Upstream is not going to bump to LDK 0.3 on lampo's timeline (decision in
  the brainstorm), so lampo owns the LDK-to-hsmd mapping.

## Deployment topology

```
lampod ──(fd 3 socketpair, CLN hsmd protocol)──> remote_hsmd_socket ──(gRPC, vlsd dials in)──> vlsd
                                                       │
                                                       └──(bitcoind RPC)── chain frontend (headers, TXOO proofs, heartbeats)
```

- **`remote_hsmd_socket` is a CLN `hsmd` drop-in, not a socket-path server.**
  `vls-proxy/src/socket_main.rs` calls `open_parent_fd()` and speaks the hsmd
  protocol on **file descriptor 3 inherited from its parent**. Lampo therefore
  spawns it the way `lightningd` does: create a `UnixStream::pair()`, `dup2`
  one end onto fd 3 in the child (`CommandExt::pre_exec`), keep the other end.
  Configuration is by environment: `VLS_NETWORK`, `BITCOIND_RPC_URL`,
  `VLS_PORT` (default 7701), `VLS_BIND` (default 127.0.0.1), optional
  `TXOO_SOURCE_URL`, and `REMOTE_SIGNER_ALLOWLIST` (read by `vlsd`).
- **`vlsd` dials the proxy** (`--connect http://127.0.0.1:7701 --network
  regtest --datadir <dir> [--integration-test]`), so the signer can sit behind
  NAT. The proxy retransmits outstanding requests and the cached init message
  when `vlsd` reconnects.
- **The proxy runs the chain frontend itself**, fed by `BITCOIND_RPC_URL`. Lampo
  does not implement `ChainTrack`, `AddBlock`, TXOO proofs or heartbeats in the
  first cut; it only hands the proxy a bitcoind URL. (lnrod embeds the frontend
  in-process instead; that path links `vls-frontend` and is closed to lampo.)
- **The proxy is transparent except for four things** (`grpc/signer_loop.rs`):
  `ClientHsmFd` (creates a per-channel fd), `HsmdInit`/`HsmdInit2` (cached for
  reconnect; a second init is a protocol error), and `PreapproveInvoice`/
  `PreapproveKeysend` (reply cache). Everything else is forwarded to `vlsd`
  with the fd's `(peer_id, dbid)` as context; the root fd has no context and
  is node level.

## Transport and framing

- **Message encoding** (`vls-protocol/src/msgs.rs`): `u16 BE type || body`,
  body in `serde_bolt` (big-endian ints; `Octets` = u16-length-prefixed bytes;
  `LargeOctets` = u32-prefixed; `Array<T>` = u16 count + items; `WireString`;
  `Option<T>` = presence byte + T; `WithSize<T>` = length-prefixed
  consensus-encoded T). Replies are conventionally `type + 100`.
  `SignerError` (3000) `{code: u16, message}` is the error reply;
  `CODE_ORPHAN_BLOCK = 401`.
- **Stream framing**: `u32 BE length || message`; `MAX_MESSAGE_SIZE = 128 KiB`
  (`msgs::write_vec` / `read_raw`).
- **Per-channel fds**: on the root fd send `ClientHsmFd {peer_id: PubKey,
  dbid: u64, capabilities: u64}` (9); the proxy replies `ClientHsmFdReply` (109)
  and then passes one end of a fresh socketpair over `SCM_RIGHTS` with a
  single `0xff` payload byte. Every message on that fd carries
  `ClientId {peer_id, dbid}` to the signer. One fd per channel, held for the
  channel's life; a request/reply pair is serialized per fd.
- **dbid 0 is reserved** for the node; `Transport::call` asserts `dbid != 0`.
- **Receiving an fd needs `recvmsg` with ancillary data.** `std` has no API for
  it; use `nix::sys::socket::recvmsg` (what the proxy uses) or ~30 lines of
  `libc::recvmsg`. `libc` is already in lampo's graph; `nix` is not.

## Handshake and key material

`vls-protocol-client` uses `HsmdInit2` (1011). **Lampo uses `HsmdInit` (11)
instead**, because `remote_hsmd_socket` marks its signer port ready, and so
starts its chain frontend, only after `HsmdInit`. With `HsmdInit2` the
frontend waits forever, the signer's chain tracker stays at height 0, and
the signer refuses to sign any state past the first commitment
(`ensure_funding_buried_and_unspent: funding is not buried at depth 0`), so
the first payment fails. Found running the regtest test.

```
HsmdInit { key_version: Bip32KeyVersion (CLN values per network),
           chain_params: genesis block hash, encryption_key/dev_*: None,
           hsm_wire_min_version: 4, hsm_wire_max_version: 4 }
-> HsmdInitReplyV4 (114) { hsm_version, hsm_capabilities, node_id,
                           bip32: ExtKey, bolt12: PubKey }
```

- The version is pinned to 4 on both ends: it is the last version where
  `GetPerCommitmentPoint` still releases the previous secret, which
  `release_commitment_secret` relies on (5+ moves it to
  `RevokeCommitmentTx` (40)). Lampo bails if the signer answers anything else.
- `HsmdInitReplyV4` has no `inbound_payment_key` or `peer_storage_key`
  (the LDK-only fields of `HsmdInit2Reply`). Lampo gets both from
  `DeriveSecret {info}` (27) with fixed info strings
  (`lampo/inbound_payment_key`, `lampo/peer_storage_key`): seed-derived on
  the signer, so stable across restarts and recoverable from the seed.
- `dev_*` fields are rejected unless the signer has the `developer`
  feature; allowlists come from `vlsd`'s `REMOTE_SIGNER_ALLOWLIST` file (or
  the `vls-cli` admin RPC).
- The receive-auth key is not provided by the signer; lampo generates it
  once and persists it next to the node data.
- `HsmdInit` uses the signer's configured derivation style (CLN-native in
  `vlsd`), so the node id differs from what `HsmdInit2` with
  `KeyDerivationStyle::Ldk` would give. Irrelevant for a fresh node; it
  matters only if a deployment ever switches handshakes.

## Node-level mapping (`LampoSigner`)

| LDK method | Request (type) | Reply (type) | Notes |
|---|---|---|---|
| `EntropySource::get_secure_random_bytes` | `GetSecureRandomBytes {}` (1036) | `GetSecureRandomBytesReply {random_bytes}` (1136) | Remote every call. Consider a local CSPRNG instead; entropy need not come from the signer. |
| `NodeSigner::get_expanded_key` | local | | `ExpandedKey::new(inbound_payment_key)` from the init reply. |
| `get_peer_storage_key` | local | | From the init reply. |
| `get_receive_auth_key` | local | | Generated by the node once, persisted. |
| `ecdh` | `Ecdh {point}` (1) | `EcdhReply {secret}` (100) | `Recipient::PhantomNode` and `tweak.is_some()` are unsupported in the reference client; lampo returns `Err(())`. |
| `get_node_id` | local | | From the init reply; `PhantomNode` unsupported. |
| `sign_bolt12_invoice` | `SignBolt12Invoice {invoice_bytes}` (1037) | `SignBolt12InvoiceReply {signature}` (1137) | Full `UnsignedBolt12Invoice` bytes; schnorr signature. |
| `sign_gossip_message` | `SignGossipMessage {message}` (1006) | `SignGossipMessageReply {signature}` (1106) | `msg.encode()`. |
| `sign_invoice` | `SignInvoice {u5bytes, hrp}` (8) | `SignInvoiceReply {signature: 65 bytes}` (108) | Byte 64 is the recovery id. |
| `sign_message` | `SignMessage {message}` (23) | `SignMessageReply {signature: 65 bytes}` (123) | zbase32 of the recoverable signature. |
| `SignerProvider::generate_channel_keys_id` | local | | `dbid = next_dbid.fetch_add(1)` starting at 1; `channel_keys_id = 24 zero bytes ‖ dbid LE`. See persistence. |
| `derive_channel_signer` | `NewChannel {peer_id, dbid}` (30), then `GetChannelBasepoints {node_id, dbid}` (10), both on the **root** fd | `NewChannelReply` (130), `GetChannelBasepointsReply {basepoints, funding}` (110) | `dbid` = last 8 bytes of `channel_keys_id` LE. `peer_id = [0; 33]`: LDK gives no peer at derive time. Then `ClientHsmFd` for the channel's own fd. Sending these two on the channel fd makes `vlsd` abort ("unimplemented message"). |
| `get_destination_script` | local | | Reference client derives `xpub/1` P2WPKH from the init reply. Lampo instead returns the wallet script (see policy). |
| `get_shutdown_scriptpubkey` | local | | Same source as above. |
| `OutputSpender::spend_spendable_outputs` | `SignWithdrawal {utxos: Array<Utxo>, psbt}` (7) | `SignWithdrawalReply {psbt}` (107) | Build the tx locally, set `witness_utxo` per input, map each descriptor to `Utxo {txid, outnum, amount, keyindex, is_p2sh, script, close_info: Option<CloseInfo{channel_id: dbid, peer_id: [0;33], commitment_point, is_anchors, csv}>, is_in_coinbase}`. `StaticOutput` → `keyindex = 1`, no `close_info`; `DelayedPaymentOutput` → `commitment_point = Some`, `csv = to_self_delay`; `StaticPaymentOutput` → `commitment_point = None`. Copy witnesses from the reply. |
| `ChangeDestinationSourceSync::get_change_destination_script` | local | | Wallet script. Must be allowlisted (policy). |

## Channel-level mapping (`LampoChannelSigner`)

All calls go over the channel's fd. LDK numbers commitments downward from
`INITIAL_COMMITMENT_NUMBER`; VLS/CLN count upward, so
`commitment_number = INITIAL_COMMITMENT_NUMBER - idx`.

| LDK method | Request (type) | Reply (type) | Notes |
|---|---|---|---|
| `get_per_commitment_point(idx)` | `GetPerCommitmentPoint2 {commitment_number}` (1018) | `GetPerCommitmentPoint2Reply {point}` (1118) | |
| `release_commitment_secret(idx)` | `GetPerCommitmentPoint {commitment_number: N(idx) + 2}` (18) | `GetPerCommitmentPointReply {point, secret: Option}` (118) | "Getting the point at idx + 2 releases the secret at idx." Requires wire version < 5; lampo pins 4. |
| `validate_holder_commitment` | `ValidateCommitmentTx2 {commitment_number, feerate, to_local_value_sat, to_remote_value_sat, htlcs, signature, htlc_signatures}` (1035) | `ValidateCommitmentTxReply {old_commitment_secret, next_per_commitment_point}` (135) | Before `SetupChannel` the reference client **defers** this call (one slot) and replays it after setup. Preimages are ignored upstream. |
| `validate_counterparty_revocation` | `ValidateRevocation {commitment_number, commitment_secret}` (36) | `ValidateRevocationReply` (136) | |
| `pubkeys` | local | | Cached from `GetChannelBasepointsReply`. |
| `new_funding_pubkey` | none | | Splicing; `todo!()` upstream. Lampo returns an error path (see gaps). |
| `channel_keys_id` | local | | |
| `sign_counterparty_commitment` | `SignRemoteCommitmentTx2 {remote_per_commitment_point, commitment_number, feerate, to_local_value_sat, to_remote_value_sat, htlcs}` (1019) | `SignCommitmentTxWithHtlcsReply {signature, htlc_signatures}` (1119) | `ensure_channel_setup` first. Values are from the remote's view: `to_local = to_countersignatory`, `to_remote = to_broadcaster`. `Htlc {side, amount_msat, payment_hash, ctlv_expiry}` with `side = LOCAL` if `offered != is_remote`. |
| `sign_holder_commitment` | `SignLocalCommitmentTx2 {commitment_number}` (1005) | `SignCommitmentTxReply {signature}` (105) | `ensure_channel_setup` first. The signer signs from its own state; it does not take the tx. |
| `sign_justice_revoked_output` | none in the reference client | | Candidates: `SignPenaltyToUs {revocation_secret, tx, psbt, wscript}` (14) → `SignTxReply` (112); node-level `SignAnyPenaltyToUs` (144). |
| `sign_justice_revoked_htlc` | none in the reference client | | Same candidates. |
| `sign_holder_htlc_transaction` | `SignLocalHtlcTx2 {tx, input, per_commitment_number, offered, cltv_expiry, htlc_amount_msat, payment_hash}` (20) | `SignTxReply` (112) | No `ensure_channel_setup` upstream. |
| `sign_counterparty_htlc_transaction` | none in the reference client | | Candidates: `SignRemoteHtlcToUs {remote_per_commitment_point, tx, psbt, wscript, option_anchors}` (13); node-level `SignAnyRemoteHtlcToUs` (143). |
| `sign_closing_transaction` | `SignMutualCloseTx2 {to_local_value_sat, to_remote_value_sat, local_script, remote_script, local_wallet_path_hint}` (1021) | `SignTxReply` (112) | `ensure_channel_setup` first. `policy-mutual-destination-allowlisted`: the local script must be wallet-derivable at the hint path or allowlisted. |
| `sign_holder_keyed_anchor_input` | none in the reference client | | Candidate: node-level `SignAnchorspend {peer_id, dbid, utxos, psbt}` (147) → `SignAnchorspendReply {psbt}` (148). |
| `sign_channel_announcement_with_funding_key` | `SignChannelAnnouncement {announcement}` (2) | `SignChannelAnnouncementReply {node_signature, bitcoin_signature}` (102) | Prepend 258 zero bytes (CLN framing); return `bitcoin_signature`. `ensure_channel_setup` first. |
| `sign_splice_shared_input` | none | | `SignSpliceTx` (29) exists but its handler is a stub and it is not advertised. |

**Gaps inherited from upstream.** The reference client leaves six methods as
`todo!()`: both justice signers, `sign_counterparty_htlc_transaction`,
`sign_holder_keyed_anchor_input`, `sign_splice_shared_input`, and
`new_funding_pubkey`. A lampo node with those gaps can open, pay, and
cooperatively close, but cannot punish a breach or claim counterparty HTLC
outputs on chain. Lampo must return `Err(())` rather than panic there (LDK
treats `Err` as "pending", and lampo never calls `signer_unblocked`, so the
claim stalls but the node stays up) and must **refuse to start with the VLS
signer on mainnet** until the penalty and HTLC-to-us paths are implemented over
`SignPenaltyToUs` / `SignRemoteHtlcToUs`.

## Channel setup sequence

1. `generate_channel_keys_id` → local `dbid`, no traffic.
2. `derive_channel_signer(channel_keys_id)` → `NewChannel` (30) and
   `GetChannelBasepoints` (10) on the **root** fd (they are node-level; the
   signer's per-channel handler panics on them), then `ClientHsmFd
   {peer_id: [0;33], dbid}` on the root fd to receive the channel fd that
   every later signing call uses. Signer side,
   `Node::new_channel(dbid, peer_id)` is idempotent for a known `(peer, dbid)`
   and errors with `policy-channel-original-channel-id-reuse` if
   `dbid <= dbid_high_water_mark` (raised by `ForgetChannel`).
3. **`SetupChannel` (31) is lazy.** LDK 0.2 removed
   `provide_channel_parameters`, so the reference client sends it from
   `ensure_channel_setup(&ChannelTransactionParameters)` on the first of
   `sign_counterparty_commitment`, `sign_holder_commitment`,
   `sign_closing_transaction`, `sign_channel_announcement_with_funding_key`,
   holding a per-signer lock across the round trip and any deferred
   `validate_holder_commitment` replay. Fields:

   ```
   SetupChannel { is_outbound, channel_value, push_value: 0,
     funding_txid, funding_txout: u16, to_self_delay: holder_selected_contest_delay,
     local_shutdown_script: empty, local_shutdown_wallet_index: None,
     remote_basepoints: {revocation, payment, htlc, delayed_payment},
     remote_funding_pubkey, remote_to_self_delay: counterparty.selected_contest_delay,
     remote_shutdown_script: empty, channel_type: Octets } -> SetupChannelReply (131)
   ```

   `channel_type` is the feature-bit encoding of `StaticRemoteKey`, `Anchors`
   (rejected by `policy-channel-safe-type`) or `AnchorsZeroFeeHtlc`. Returns an
   error if `funding_outpoint` or `counterparty_parameters` is `None`. A repeat
   with identical parameters is accepted; a different one is rejected.
4. There is no `ReadyChannel` message; readiness is the signer-side
   `ChannelSlot::Ready` state after `SetupChannel`.

`ChannelId::new_from_oid(dbid)` = 24 zero bytes + `dbid.to_le_bytes()`;
`ChannelId::new(&keys_id).oid()` reads the last 8 bytes LE.

## Persistence and restart

- The signer persists channel state (stub/ready, tracker) in its own
  persister. Lampo keeps nothing signer-specific inside LDK state: channel
  signers are never serialized and `read_channel_monitors` re-derives them
  through `derive_channel_signer`, which re-sends `ClientHsmFd`, `NewChannel`
  and `GetChannelBasepoints`; `SetupChannel` is re-sent lazily on the first
  signing call and accepted if identical.
- **Lampo must persist two things the reference client does not:** the
  `next_dbid` counter (upstream restarts at 1, relying on the signer returning
  the existing stub; a dbid that was `ForgetChannel`ed would then trip the
  high-water mark) and the `ReceiveAuthKey`. Both go in a small file under the
  lampo data dir.

## Chain feed obligations

`remote_hsmd_socket` runs the `vls-frontend` `Frontend` itself against
`BITCOIND_RPC_URL`, using `SignerPortFront`/`NodePortFront` over node-level
messages: `NodeInfo` (1012), `TipInfo` (2002), `ForwardWatches` (2003),
`ReverseWatches` (2004), `AddBlock {header, unspent_proof}` (2005),
`RemoveBlock` (2006), `BlockChunk` (2009), `GetHeartbeat` (2008), `Ping`
(1000). Lampo does none of this in the first cut. Consequences:

- The proxy needs the same bitcoind lampo uses; lampo passes its `core_url`
  and credentials through the environment when spawning.
- Heartbeats go stale after 5 s on regtest/signet and 3600 s on mainnet; the
  proxy handles them.
- A TXOO source (`TXOO_SOURCE_URL`) is optional; without it the proxy uses a
  dummy source and the signer validates with reduced on-chain assurance.

## Policy: what the signer will refuse

- `policy-onchain-no-unknown-outputs` / `policy-sweep-destination-allowlisted`:
  every output of a `SignWithdrawal` must be derivable from the signer's own
  wallet xpub at the PSBT's `bip32_derivation` path, or be in the allowlist.
- `policy-mutual-destination-allowlisted`: the local script in
  `SignMutualCloseTx2` likewise.

Lampo sources destination, shutdown and change scripts from the BDK wallet,
which the signer knows nothing about. Therefore the **BDK wallet's account
xpub must be in the signer's allowlist** (`Allowable::XPub`), or closes and
sweeps are rejected. The test harness writes the wallet tpub into the
`REMOTE_SIGNER_ALLOWLIST` file; production operators add it with `vls-cli`.
An xpub entry only matches when the signer is told the derivation path of the
script relative to that xpub (`allowlist_contains(script, path)` derives
`xpub/path` and compares; an empty path never matches an xpub). And the
signer checks its *own* wallet with the same path first, which errors on any
path longer than one component (`get_wallet_key: bad child_path len`), failing
the whole check before the allowlist is consulted. So the path can only be
the address index, and the allowlist must hold one xpub per BDK keychain
(`account/0` external, `account/1` change) rather than the account xpub.
Found running the regtest close test.

The `WalletManager` trait grew two default methods, implemented by the BDK
wallet:

- `account_xpub()`: the key both keychains hang off (`m/84'/1'/0'` for the
  BIP84 template). `lampo_vls::wallet_allowlist` turns it into the two
  keychain entries.
- `script_derivation(script)`: `(keychain, index)` of a revealed address.

`lampo-vls` passes `[index]` as `local_wallet_path_hint` in
`SignMutualCloseTx2`, and writes it as `bip32_derivation` on every sweep
output that is ours before sending `SignWithdrawal` (the signer reads only the
path). `VlsSigner::set_wallet` logs the exact entries to add.

A hot BDK wallet next to a cold channel signer is the accepted first-cut
model; moving L1 into VLS (`SignWithdrawal` for wallet spends, watch-only BDK)
is a later phase. Outbound payments are not preapproved (`PreapproveInvoice`
is a CLN-driven message lampo does not send yet), so `vlsd` must run with
`VLS_AUTOAPPROVE=1` for now; sending preapprovals from `pay` is phase 3 work.

## Proposed `lampo-vls` crate

```
lampo-vls/
  src/lib.rs        VlsSigner: impl LampoSigner; VlsSignerConfig; spawn + init
  src/transport.rs  Hsmd fd transport: socketpair, dup2 to fd 3, framing,
                    ClientHsmFd + SCM_RIGHTS, per-dbid Mutex<UnixStream>
  src/node.rs       NodeSigner / EntropySource / OutputSpender over the root fd
  src/channel.rs    VlsChannelSigner: impl EcdsaChannelSigner, lazy SetupChannel
  src/convert.rs    LDK <-> vls-protocol model conversions (Htlc, Basepoints,
                    commitment numbering, channel_type bits, Utxo/CloseInfo)
  src/state.rs      persisted next_dbid + receive_auth_key
```

- **Dependencies**: `vls-protocol = "1.0.0-rc.1"` (brings `lightning 0.2.x` as
  a second version; documented above), `libc` (already in the graph) for
  `dup2`/`recvmsg`, `hex`, `lampo-common`. No tokio inside the signer:
  every call is blocking on a `Mutex<UnixStream>`, which matches LDK's sync
  traits and the daemon's embeddability requirement.
- **Wiring**: `LampoConf` gains `signer` (`in-memory` default, `vls`),
  `vls_proxy_bin`, `vls_port`. `lampod-cli` built with `--features vls`
  builds `VlsSigner::spawn(conf)` and calls `LampoDaemon::with_signer`;
  without the feature `signer=vls` is a startup error. The BDK wallet keeps
  its own seed for L1.
- **Mainnet gate**: `VlsSigner::spawn` bails on `Network::Bitcoin` until the
  justice and HTLC-to-us signers exist. Logged at startup with the gap list.
- **Phases**:
  1. Transport, handshake, node signer, channel signer for the standard
     lifecycle (open, pay, cooperative close, sweep), persistence of
     `next_dbid` and the auth key, config, mainnet gate.
  2. Regtest harness: `lampo-testing` spawns `vlsd` and `remote_hsmd_socket`.
     Done: outbound open, pay and cooperative close to the wallet; inbound
     open and receive. Still open: a force close that drives
     `SignWithdrawal`, and a policy rejection (close to a non-allowlisted
     script). The VLS tests compile only with `--features vls` and then
     fail without `VLSD_EXE` / `REMOTE_HSMD_SOCKET_EXE`. CI runs the suite
     twice: `make check` without VLS, then `make check-vls` against the
     `vlsd` built into the Docker image.
  3. Penalty and counterparty-HTLC signing over `SignPenaltyToUs` /
     `SignRemoteHtlcToUs`; lift the mainnet gate.
  4. Keyed anchors (`SignAnchorspend`), then splicing when upstream lands it.

## Open questions and risks

- **`GetSecureRandomBytes` per call** is a round trip on a hot path
  (`OnionMessenger`, payment ids). Use a local `OsRng` instead; entropy need
  not be signer-provided.
- **`SetupChannel` before the first validation** on the inbound side: LDK
  calls `validate_holder_commitment` before any signing op, so the deferred
  replay from the reference client must be reproduced exactly, including the
  single-slot limitation.
- **fd exhaustion**: one fd per channel for the daemon's lifetime, plus the
  proxy's own. Fine for hundreds of channels; document the ulimit.
- **TXOO source**: without `TXOO_SOURCE_URL` the proxy uses a dummy source
  (written into its data dir) and the signer trusts bitcoind's view of the
  chain. Point it at a TXOO oracle for mainnet.
- **Proxy restarts**: if `remote_hsmd_socket` dies the root fd breaks and every
  channel fd with it. First cut: fail every signing call with `Err(())` and
  log; a supervisor that respawns and re-runs `HsmdInit` is a follow-up. The
  proxy itself already survives `vlsd` reconnects.
- **`hsm_wire_version`**: lampo pins version 4. If VLS ever drops version
  4, `release_commitment_secret` must move to `RevokeCommitmentTx` (40) and
  the pin to 5 or 6.
- **Two `lightning` versions with `--features vls`**: confined to the VLS
  build by the feature gate; the default build and the LDK version guard are
  unaffected. Revisit if `vls-protocol` ever drops its `vls-core`
  dependency, which would remove the duplicate and the need for the gate.
