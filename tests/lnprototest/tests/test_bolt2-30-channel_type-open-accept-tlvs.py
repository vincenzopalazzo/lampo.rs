import pytest

from typing import Any

from lnprototest import (
    TryAll,
    Connect,
    Block,
    ExpectMsg,
    OneOf,
    Msg,
    Runner,
    KeySet,
    bitfield,
    channel_type_csv,
    ExpectError,
    ExpectDisconnect,
)
from lnprototest.utils import (
    BitcoinUtils,
    tx_spendable,
    funding_amount_for_utxo,
    pubkey_of,
)


def test_open_channel(runner: Runner, with_proposal: Any) -> None:
    """Tests for https://github.com/lightningnetwork/lightning-rfc/pull/880"""
    # FIXME: this is cln dependent we should not do this!
    with_proposal(channel_type_csv)

    local_funding_privkey = "20"

    local_keyset = KeySet(
        revocation_base_secret="21",
        payment_base_secret="22",
        htlc_base_secret="24",
        delayed_payment_base_secret="23",
        shachain_seed="00" * 32,
    )

    test = [
        Block(blockheight=102, txs=[tx_spendable]),
        Connect(connprivkey="02"),
        ExpectMsg("init"),
        Msg("init", globalfeatures="", features=bitfield(0, 8, 12, 14)),
        Msg(
            "open_channel",
            chain_hash=BitcoinUtils.blockchain_hash(),
            temporary_channel_id="00" * 32,
            funding_satoshis=funding_amount_for_utxo(0),
            push_msat=0,
            dust_limit_satoshis=546,
            max_htlc_value_in_flight_msat=4294967295,
            channel_reserve_satoshis=9998,
            htlc_minimum_msat=0,
            feerate_per_kw=253,
            # We use 5, because core-lightning runner uses 6, so this is different.
            to_self_delay=5,
            max_accepted_htlcs=483,
            funding_pubkey=pubkey_of(local_funding_privkey),
            revocation_basepoint=local_keyset.revocation_basepoint(),
            payment_basepoint=local_keyset.payment_basepoint(),
            delayed_payment_basepoint=local_keyset.delayed_payment_basepoint(),
            htlc_basepoint=local_keyset.htlc_basepoint(),
            first_per_commitment_point=local_keyset.per_commit_point(0),
            channel_flags=1,
            # We negotiate *down* to a simple static channel
            tlvs="{channel_type={type=" + bitfield(12) + "}}",
        ),
        # BOLT #2
        #   - if it sets `channel_type`:
        #     - MUST set it to the `channel_type` from `open_channel`
        ExpectMsg("accept_channel", tlvs="{channel_type={type=" + bitfield(12) + "}}"),
    ]

    runner.run(test)


# DEBUG    root:bitcoind.py:43 Result for getblockcount call: 102
# DEBUG    root:structure.py:49 receiving event {"event": "Connect", "file": "test_bolt2-30-channel_type-open-accept-tlvs.py", "pos": "96"}
# INFO     root:event.py:59 # running {'event': 'Connect', 'file': 'test_bolt2-30-channel_type-open-accept-tlvs.py', 'pos': '96'}:
@pytest.mark.skip(reason="the connection get stuck, look at the coment in the code")
def test_open_channel_bad_type(runner: Runner, with_proposal: Any) -> None:
    """Tests for https://github.com/lightningnetwork/lightning-rfc/pull/880"""
    with_proposal(channel_type_csv)

    local_funding_privkey = "20"

    local_keyset = KeySet(
        revocation_base_secret="21",
        payment_base_secret="22",
        htlc_base_secret="24",
        delayed_payment_base_secret="23",
        shachain_seed="00" * 32,
    )

    test = [
        Block(blockheight=102, txs=[tx_spendable]),
        Connect(connprivkey="02"),
        ExpectMsg("init"),
        TryAll(
            # BOLT-a12da24dd0102c170365124782b46d9710950ac1 #9:
            # | 20/21 | `option_anchor_outputs`          | Anchor outputs
            Msg("init", globalfeatures="", features=bitfield(12, 21)),
            # BOLT #9:
            # | 12/13 | `option_static_remotekey`        | Static key for remote output
            Msg("init", globalfeatures="", features=bitfield(12)),
            # And not.
            Msg("init", globalfeatures="", features=""),
        ),
        Msg(
            "open_channel",
            chain_hash=BitcoinUtils.blockchain_hash(),
            temporary_channel_id="00" * 32,
            funding_satoshis=funding_amount_for_utxo(0),
            push_msat=0,
            dust_limit_satoshis=546,
            max_htlc_value_in_flight_msat=4294967295,
            channel_reserve_satoshis=9998,
            htlc_minimum_msat=0,
            feerate_per_kw=253,
            # We use 5, because core-lightning runner uses 6, so this is different.
            to_self_delay=5,
            max_accepted_htlcs=483,
            funding_pubkey=pubkey_of(local_funding_privkey),
            revocation_basepoint=local_keyset.revocation_basepoint(),
            payment_basepoint=local_keyset.payment_basepoint(),
            delayed_payment_basepoint=local_keyset.delayed_payment_basepoint(),
            htlc_basepoint=local_keyset.htlc_basepoint(),
            first_per_commitment_point=local_keyset.per_commit_point(0),
            channel_flags=1,
            tlvs="{channel_type={type=" + bitfield(100) + "}}",
        ),
        # BOLT #2
        # The receiving node MUST fail the channel if:
        #   - It supports `channel_types` and none of the `channel_types`
        #     are suitable.
        # FIXME: check if ldk is protocol compliant
        ExpectDisconnect(),
    ]

    runner.run(test)
