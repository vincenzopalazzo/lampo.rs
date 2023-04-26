"""
LNPrototest Runner implementation for lampo.

It is an experimental version of a test interop
for ldk implementation.


Autor: Vincenzo Palazzo <vincenzopalazzodev@gmail.com>
"""
import shutil
from typing import Any, Union, Optional, Sequence, List

from lnprototest import KeySet
from lnprototest.runner import Runner
from lnprototest.event import Event, MustNotMsg, ExpectMsg


class Conn(object):
    """Class for connections.  Details filled in by the particular runner.

    But overall this is for manging the connectiong between the runner -> node implementation
    in fact this will init the connection when you will expect that lnprototest
    is calling a real connect method.

    In reality we simulate the init workflow with the pyln packages that implement a
    modular python implementation for the lightning network building blocks.
    """

    def __init__(self, connprivkey: str):
        """Create a connection from a node with the given hex privkey: we use
        trivial values for private keys, so we simply left-pad with zeroes"""
        self.name = connprivkey
        self.connprivkey = privkey_expand(connprivkey)
        self.pubkey = coincurve.PublicKey.from_secret(self.connprivkey.secret)
        self.expected_error = False
        self.must_not_events: List[MustNotMsg] = []

    def __str__(self) -> str:
        return self.name


class LampoRunner(Runner):
    """
    Lampo Runner implementation, this is the entry point
    of runner implementation, so all the lampo interaction
    happens here!
    """

    def __init__(self, config: Any) -> None:
        super().__init__(config)
        self.config = config

    def check_error(self, event: Event, conn: Conn) -> Optional[str]:
        conn.expected_error = True
        return None

    def is_running(self) -> bool:
        """Return a boolean value that tells whether the runner is running
        or not.
        Is leave up to the runner implementation to keep the runner state"""
        pass

    def connect(self, event: Event, connprivkey: str) -> None:
        pass

    def check_final_error(
        self,
        event: Event,
        conn: Conn,
        expected: bool,
        must_not_events: List[MustNotMsg],
    ) -> None:
        pass

    def start(self) -> None:
        pass

    def stop(self, print_logs: bool = False) -> None:
        """
        Stop the runner, and print all the log that the ln
        implementation produced.
        Print the log is useful when we have a failure e we need
        to debug what happens during the tests.
        """
        pass

    def recv(self, event: Event, conn: Conn, outbuf: bytes) -> None:
        pass

    def get_output_message(self, conn: Conn, event: ExpectMsg) -> Optional[bytes]:
        pass

    def getblockheight(self) -> int:
        pass

    def trim_blocks(self, newheight: int) -> None:
        pass

    def add_blocks(self, event: Event, txs: List[str], n: int) -> None:
        pass

    def expect_tx(self, event: Event, txid: str) -> None:
        pass

    def invoice(self, event: Event, amount: int, preimage: str) -> None:
        pass

    def accept_add_fund(self, event: Event) -> None:
        pass

    def fundchannel(
        self,
        event: Event,
        conn: Conn,
        amount: int,
        feerate: int = 0,
        expect_fail: bool = False,
    ) -> None:
        pass

    def init_rbf(
        self,
        event: Event,
        conn: Conn,
        channel_id: str,
        amount: int,
        utxo_txid: str,
        utxo_outnum: int,
        feerate: int,
    ) -> None:
        pass

    def addhtlc(self, event: Event, conn: Conn, amount: int, preimage: str) -> None:
        pass

    def get_keyset(self) -> KeySet:
        pass

    def get_node_privkey(self) -> str:
        pass

    def get_node_bitcoinkey(self) -> str:
        pass

    def has_option(self, optname: str) -> Optional[str]:
        pass

    def add_startup_flag(self, flag: str) -> None:
        pass

    def close_channel(self, channel_id: str) -> None:
        """
        Close the channel with the specified channel id.
        :param channel_id:  the channel id as string value where the
        caller want to close;
        :return No value in case of success is expected,
        but an `RpcError` is expected in case of err.
        """
        pass
