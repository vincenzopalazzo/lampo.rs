"""
LNPrototest Runner implementation for lampo.

It is an experimental version of a test interop
for ldk implementation.


Autor: Vincenzo Palazzo <vincenzopalazzodev@gmail.com>
"""
import os
import tempfile
import logging
import socket
import shutil

import pyln

from typing import Any, Optional, List, cast

from contextlib import closing
from concurrent import futures

from lnprototest import KeySet, Conn
from lnprototest.backend import Bitcoind
from lnprototest.runner import Runner
from lnprototest.event import Event, MustNotMsg

from lampo_py import LampoDeamon

# FIXME: move this in the Runner
TIMEOUT = int(os.getenv("TIMEOUT", "30"))


class LampoConn(Conn):
    def __init__(self, connprivkey: str, public_key: str, port: int):
        super().__init__(connprivkey)
        # FIXME: pyln.proto.wire should just use coincurve PrivateKey!
        self.connection = pyln.proto.wire.connect(
            pyln.proto.wire.PrivateKey(bytes.fromhex(self.connprivkey.to_hex())),
            pyln.proto.wire.PublicKey(bytes.fromhex(public_key)),
            "127.0.0.1",
            port,
        )


class LampoRunner(Runner):
    """
    Lampo Runner implementation, this is the entry point
    of runner implementation, so all the lampo interaction
    happens here!
    """

    def __init__(self, config: Any) -> None:
        """Init the runner."""
        super().__init__(config)
        self.directory = tempfile.mkdtemp(prefix="lnpt-lampo-")
        self.config = config
        self.lightning_port = self.reserve_port()
        self.__lampod_config_file()
        self.node = LampoDeamon(self.directory)
        self.last_conn = None
        self.public_key = None
        self.bitcoind = None
        self.executor = futures.ThreadPoolExecutor(max_workers=20)

    def __lampod_config_file(self) -> None:
        f = open(f"{self.directory}/lampo.conf", "w")
        f.write(
            f"network=testnet\nport={self.lightning_port}\ndev-private-key=0000000000000000000000000000000000000000000000000000000000000001\ndev-force-channel-secrets={self.get_node_bitcoinkey()}/0000000000000000000000000000000000000000000000000000000000000010/0000000000000000000000000000000000000000000000000000000000000011/0000000000000000000000000000000000000000000000000000000000000012/0000000000000000000000000000000000000000000000000000000000000013/0000000000000000000000000000000000000000000000000000000000000014/FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF"
        )
        f.flush()
        f.close()

    # FIXME: move this in lnprototest runner API
    def reserve_port(self) -> int:
        """
        Reserve a port.

        When python asks for a free port from the os, it is possible that
        with concurrent access, the port that is picked is a port that is not free
        anymore when we go to bind the daemon like bitcoind port.

        Source: https://stackoverflow.com/questions/1365265/on-localhost-how-do-i-pick-a-free-port-number
        """
        with closing(socket.socket(socket.AF_INET, socket.SOCK_STREAM)) as s:
            s.bind(("", 0))
            s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            return s.getsockname()[1]

    def listen(self) -> None:
        self.node.listen()

    def check_error(self, event: Event, conn: Conn) -> Optional[str]:
        conn.expected_error = True
        return None

    def is_running(self) -> bool:
        """Return a boolean value that tells whether the runner is running
        or not.
        Is leave up to the runner implementation to keep the runner state"""
        pass

    def connect(self, event: Event, connprivkey: str) -> None:
        self.add_conn(LampoConn(connprivkey, self.public_key, self.lightning_port))

    def disconnect(self, event: Event, conn: Conn) -> None:
        if conn is None:
            raise SpecFileError(event, "Unknown conn")
        del self.conns[conn.name]
        self.check_final_error(event, conn, conn.expected_error, conn.must_not_events)

    def check_final_error(
        self,
        event: Event,
        conn: Conn,
        expected: bool,
        must_not_events: List[MustNotMsg],
    ) -> None:
        pass

    def start(self) -> None:
        """Start the Runner."""
        self.bitcoind = Bitcoind(self.directory)
        try:
            self.bitcoind.start()
        except Exception as ex:
            logging.debug(f"Exception with message {ex}")
        logging.debug("RUN Bitcoind")

        self.public_key = self.node.call("getinfo", {})["node_id"]
        self.running = True
        logging.info(f"run lampod with node id {self.public_key}")

    def shutdown(self, also_bitcoind: bool = True) -> None:
        # FIXME: stop the lightning node.
        if also_bitcoind:
            self.bitcoind.stop()

    def stop(self, print_logs: bool = False) -> None:
        """
        Stop the runner.

        The function will print all the log that the ln
        implementation produced.
        Print the log is useful when we have a failure e we need
        to debug what happens during the tests.
        """
        self.shutdown(also_bitcoind=True)
        self.running = False
        for c in self.conns.values():
            cast(LampoConn, c).connection.connection.close()

    def recv(self, event: Event, conn: Conn, outbuf: bytes) -> None:
        pass

    # FIXME: this can stay in the runner interface?
    def get_output_message(
        self, conn: Conn, event: Event, timeout: int = TIMEOUT
    ) -> Optional[bytes]:
        fut = self.executor.submit(cast(LampoConn, conn).connection.read_message)
        try:
            return fut.result(timeout)
        except futures.TimeoutError as ex:
            logging.error(f"timeout exception {ex}")
            return None
        except Exception as ex:
            logging.error(f"{ex}")
            return None

    def getblockheight(self) -> int:
        return self.bitcoind.rpc.getblockcount()

    def trim_blocks(self, newheight: int) -> None:
        h = self.bitcoind.rpc.getblockhash(newheight + 1)
        self.bitcoind.rpc.invalidateblock(h)

    def add_blocks(self, event: Event, txs: List[str], n: int) -> None:
        for tx in txs:
            self.bitcoind.rpc.sendrawtransaction(tx)
        self.bitcoind.rpc.generatetoaddress(n, self.bitcoind.rpc.getnewaddress())

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
        return "01"

    def get_node_bitcoinkey(self) -> str:
        return "0000000000000000000000000000000000000000000000000000000000000010"

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
