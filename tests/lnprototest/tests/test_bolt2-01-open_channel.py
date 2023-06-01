# Variations on open_channel, accepter + opener perspectives
import logging
import traceback

import coincurve
import bitcoin.core

from typing import List, Tuple, Union, Sequence

from lnprototest import (
    TryAll,
    KeySet,
    Event,
    Block,
    FundChannel,
    ExpectMsg,
    ExpectTx,
    Msg,
    RawMsg,
    AcceptFunding,
    CreateFunding,
    Commit,
    Runner,
    remote_funding_pubkey,
    remote_revocation_basepoint,
    remote_payment_basepoint,
    remote_htlc_basepoint,
    remote_per_commitment_point,
    remote_delayed_payment_basepoint,
    Connect,
    Side,
    CheckEq,
    msat,
    remote_funding_privkey,
    regtest_hash,
    bitfield,
    privkey_expand,
)

from lnprototest.stash import (
    sent,
    rcvd,
    commitsig_to_send,
    commitsig_to_recv,
    channel_id,
    funding_txid,
    funding_tx,
    funding,
)

tx_spendable = "0200000000010184591a56720aabc8023cecf71801c5e0f9d049d0c550ab42412ad12a67d89f3a0000000000feffffff0780841e0000000000160014fd9658fbd476d318f3b825b152b152aafa49bc9240420f000000000016001483440596268132e6c99d44dae2d151dabd9a2b232c180a2901000000160014d295f76da2319791f36df5759e45b15d5e105221c0c62d000000000016001454d14ae910793e930d8e33d3de0b0cbf05aa533300093d00000000001600141b42e1fc7b1cd93a469fa67ed5eabf36ce354dd620a107000000000016001406afd46bcdfd22ef94ac122aa11f241244a37ecc808d5b000000000022002000b068df6e0e0542e776cea5ebe8f5f1a9b40b531ddd8e94b1a7ff9829b5bbaa024730440220367b9bfed0565bad2137124f736373626fa3135e59b20a7b5c1d8f2b8f1b26bb02202f664de39787082a376d222487f02ef19e45696c041044a6d579eecabb68e94501210356609a904a7026c7391d3fbf71ad92a00e04b4cd2fb6a8d1e69cbc0998f6690a65000000"


# FIXME: move this in a util module of lnprototest
def get_traceback(e: Exception) -> str:
    lines = traceback.format_exception(type(e), e, e.__traceback__)
    return "".join(lines)


def merge_events_sequences(
    pre: Union[Sequence, List[Event], Event], post: Union[Sequence, List[Event], Event]
) -> Union[Sequence, List[Event], Event]:
    """Merge the two list in the pre-post order"""
    pre.extend(post)
    return pre


def run_runner(runner: Runner, test: Union[Sequence, List[Event], Event]) -> None:
    """
    The pytest using the assertion as safe failure, and the exception it is only
    an event that must not happen.

    From design, lnprototest fails with an exception, and for this reason, if the
    lnprototest throws an exception, we catch it, and we fail with an assent.
    """
    try:
        runner.run(test)
    except Exception as ex:
        runner.stop(print_logs=True)
        logging.error(get_traceback(ex))
        assert False, ex


def funding_amount_for_utxo(index: int = 0) -> int:
    """How much can we fund a channel for using utxo #index?"""
    _, _, amt, _, fee = utxo(index)
    return amt - fee


def gen_random_keyset(counter: int = 20) -> KeySet:
    """Helper function to generate a random keyset."""
    return KeySet(
        revocation_base_secret=f"{counter + 1}",
        payment_base_secret=f"{counter + 2}",
        htlc_base_secret=f"{counter + 3}",
        delayed_payment_base_secret=f"{counter + 4}",
        shachain_seed="00" * 32,
    )


def connect_to_node_helper(
    runner: Runner,
    tx_spendable: str,
    conn_privkey: str = "02",
    global_features="",
    features: str = "",
) -> List[Event]:
    """Helper function to make a connection with the node"""
    return [
        Block(blockheight=102, txs=[tx_spendable]),
        Connect(connprivkey=conn_privkey),
        ExpectMsg("init"),
        Msg("init", globalfeatures=global_features, features=features),
    ]


def pubkey_of(privkey: str) -> str:
    """Return the public key corresponding to this privkey"""
    return (
        coincurve.PublicKey.from_secret(privkey_expand(privkey).secret).format().hex()
    )


def utxo(index: int = 0) -> Tuple[str, int, int, str, int]:
    """Helper to get a P2WPKH UTXO, amount, privkey and fee from the tx_spendable transaction"""

    amount = (index + 1) * 1000000
    if index == 0:
        txout = 1
        key = "76edf0c303b9e692da9cb491abedef46ca5b81d32f102eb4648461b239cb0f99"
    elif index == 1:
        txout = 0
        key = "bc2f48a76a6b8815940accaf01981d3b6347a68fbe844f81c50ecbadf27cd179"
    elif index == 2:
        txout = 3
        key = "16c5027616e940d1e72b4c172557b3b799a93c0582f924441174ea556aadd01c"
    elif index == 3:
        txout = 4
        key = "53ac43309b75d9b86bef32c5bbc99c500910b64f9ae089667c870c2cc69e17a4"
    elif index == 4:
        txout = 2
        key = "16be98a5d4156f6f3af99205e9bc1395397bca53db967e50427583c94271d27f"
        amount = 4983494700
    elif index == 5:
        txout = 5
        key = "0000000000000000000000000000000000000000000000000000000000000002"
        amount = 500000
    elif index == 6:
        txout = 6
        key = "38204720bc4f9647fd58c6d0a4bd3a6dd2be16d8e4273c4d1bdd5774e8c51eaf"
        amount = 6000000
    else:
        raise ValueError("index must be 0-6 inclusive")

    # Reasonable funding fee in sats
    reasonable_funding_fee = 200

    return txid_raw(tx_spendable), txout, amount, key, reasonable_funding_fee


def txid_raw(tx: str) -> str:
    """Helper to get the txid of a tx: note this is in wire protocol order, not bitcoin order!"""
    return bitcoin.core.CTransaction.deserialize(bytes.fromhex(tx)).GetTxid().hex()


def test_open_channel_from_accepter_side(runner: Runner) -> None:
    """Check the open channel from an accepter view point"""
    local_funding_privkey = "20"
    local_keyset = gen_random_keyset(int(local_funding_privkey))
    connections_events = connect_to_node_helper(
        runner=runner, tx_spendable=tx_spendable, conn_privkey="02"
    )

    # Accepter side: we initiate a new channel.
    test_events = [
        Msg(
            "open_channel",
            chain_hash=regtest_hash,
            temporary_channel_id="00" * 32,
            funding_satoshis=funding_amount_for_utxo(0),
            push_msat=0,
            dust_limit_satoshis=546,
            max_htlc_value_in_flight_msat=4294967295,
            channel_reserve_satoshis=9998,
            htlc_minimum_msat=0,
            feerate_per_kw=253,
            # We use 5, because c-lightning runner uses 6, so this is different.
            to_self_delay=5,
            max_accepted_htlcs=483,
            funding_pubkey=pubkey_of(local_funding_privkey),
            revocation_basepoint=local_keyset.revocation_basepoint(),
            payment_basepoint=local_keyset.payment_basepoint(),
            delayed_payment_basepoint=local_keyset.delayed_payment_basepoint(),
            htlc_basepoint=local_keyset.htlc_basepoint(),
            first_per_commitment_point=local_keyset.per_commit_point(0),
            channel_flags=1,
        ),
        # Ignore unknown odd messages
        TryAll([], RawMsg(bytes.fromhex("270F"))),
        ExpectMsg(
            "accept_channel",
            temporary_channel_id=sent(),
            funding_pubkey=remote_funding_pubkey(),
            revocation_basepoint=remote_revocation_basepoint(),
            payment_basepoint=remote_payment_basepoint(),
            delayed_payment_basepoint=remote_delayed_payment_basepoint(),
            htlc_basepoint=remote_htlc_basepoint(),
            first_per_commitment_point=remote_per_commitment_point(0),
            minimum_depth=3,
            channel_reserve_satoshis=9998,
        ),
        # Ignore unknown odd messages
        TryAll([], RawMsg(bytes.fromhex("270F"))),
        # Create and stash Funding object and FundingTx
        CreateFunding(
            *utxo(0),
            local_node_privkey="02",
            local_funding_privkey=local_funding_privkey,
            remote_node_privkey=runner.get_node_privkey(),
            remote_funding_privkey=remote_funding_privkey(),
        ),
        Commit(
            funding=funding(),
            opener=Side.local,
            local_keyset=local_keyset,
            local_to_self_delay=rcvd("to_self_delay", int),
            remote_to_self_delay=sent("to_self_delay", int),
            local_amount=msat(sent("funding_satoshis", int)),
            remote_amount=0,
            local_dust_limit=546,
            remote_dust_limit=546,
            feerate=253,
            local_features=sent("init.features"),
            remote_features=rcvd("init.features"),
        ),
        Msg(
            "funding_created",
            temporary_channel_id=rcvd(),
            funding_txid=funding_txid(),
            funding_output_index=0,
            signature=commitsig_to_send(),
        ),
        ExpectMsg(
            "funding_signed",
            channel_id=channel_id(),
            signature=commitsig_to_recv(),
        ),
        # Mine it and get it deep enough to confirm channel.
        Block(blockheight=103, number=3, txs=[funding_tx()]),
        ExpectMsg(
            "channel_ready",
            channel_id=channel_id(),
            second_per_commitment_point="032405cbd0f41225d5f203fe4adac8401321a9e05767c5f8af97d51d2e81fbb206",
        ),
        Msg(
            "channel_ready",
            channel_id=channel_id(),
            second_per_commitment_point="027eed8389cf8eb715d73111b73d94d2c2d04bf96dc43dfd5b0970d80b3617009d",
        ),
        # Ignore unknown odd messages
        TryAll([], RawMsg(bytes.fromhex("270F"))),
    ]
    run_runner(runner, merge_events_sequences(connections_events, test_events))
