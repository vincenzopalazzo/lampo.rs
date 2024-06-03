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

import pyln
import shutil

from typing import Any, Optional, List, cast

from contextlib import closing
from concurrent import futures

from lnprototest import KeySet, Conn
from lnprototest import wait_for
from lnprototest.backend import Bitcoind
from lnprototest.runner import Runner
from lnprototest.event import Event, MustNotMsg

from pylampo_client import LampoClient

# FIXME: move this in the Runner
TIMEOUT = int(os.getenv("TIMEOUT", "30"))
LIGHTNING_SRC = os.path.join(os.getcwd(), os.getenv("LIGHTNING_SRC", "../.."))


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
        self.node = None
        self.last_conn = None
        self.public_key = None
        self.bitcoind = None
        self.executor = futures.ThreadPoolExecutor(max_workers=20)
        self.fundchannel_future: Optional[Any] = None
        self.cleanup_callbacks: List[Callable[[], None]] = []
        self.is_fundchannel_kill = False

    def __lampod_config_file(self) -> None:
        """Crete the lampo configuration file, and store it inside the proper directory"""
        self.lightning_dir = os.path.join(self.directory, "lampo")
        if not os.path.exists(self.lightning_dir):
            os.makedirs(self.lightning_dir)
        network_dir = os.path.join(self.lightning_dir, "regtest")
        if not os.path.exists(network_dir):
            os.makedirs(network_dir)
        self.lightning_port = self.reserve_port()
        f = open(f"{network_dir}/lampo.conf", "w")
        f.write(
            f"port={self.lightning_port}\ndev-private-key=0000000000000000000000000000000000000000000000000000000000000001\ndev-force-channel-secrets={self.get_node_bitcoinkey()}/0000000000000000000000000000000000000000000000000000000000000010/0000000000000000000000000000000000000000000000000000000000000011/0000000000000000000000000000000000000000000000000000000000000012/0000000000000000000000000000000000000000000000000000000000000013/0000000000000000000000000000000000000000000000000000000000000014/FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF\n"
        )
        # configure bitcoin core
        f.write(
            f"backend=core\ncore-url=localhost:{self.bitcoind.port}\ncore-user=rpcuser\ncore-pass=rpcpass\nnetwork=regtest"
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

    def _start_lampo(self) -> None:
        """Running the lampo node in a way that we would like to run
        and tore the rpc as `self.node`"""
        import subprocess
        import time

        process = subprocess.Popen(
            [
                "{}/target/debug/lampod-cli".format(LIGHTNING_SRC),
                "--network=regtest",
                "--dev-force-poll",
                f"--log-file={self.lightning_dir}/regtest/log.log",
                f"--data-dir={self.lightning_dir}",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

        def node_ready(rpc) -> bool:
            try:
                info = rpc.call("getinfo", None)
                logging.info(f"{info}")
                return True
            except Exception as ex:
                logging.debug(f"waiting for lampo: Exception received {ex}")
                return False

        self.node = LampoClient(f"{self.lightning_dir}/regtest/lampod.socket")
        wait_for(lambda: node_ready(self.node), timeout=TIMEOUT)
        logging.debug("Waited for lampo")

    def start(self) -> None:
        """Start the Runner."""
        self.bitcoind = Bitcoind(self.directory, with_wallet="lampo-wallet")
        try:
            self.bitcoind.start()
        except Exception as ex:
            logging.debug(f"Exception with message {ex}")
        logging.debug(f"running bitcoin core on port {self.bitcoind.port}")
        self.__lampod_config_file()
        self._start_lampo()

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
        if print_logs:
            from datetime import date

            log_path = f"{self.lightning_dir}/regtest/log.log"
            with open(log_path) as log:
                self.logger.info("---------- lampo logs ----------------")
                self.logger.info(log.read())
                # now we make a backup of the log
        #     shutil.copy(
        #        log_path,
        #       f'/tmp/lampo-log_{date.today().strftime("%b-%d-%Y_%H:%M:%S")}',
        #  )
        shutil.rmtree(self.lightning_dir)

    def recv(self, event: Event, conn: Conn, outbuf: bytes) -> None:
        try:
            cast(LampoConn, conn).connection.send_message(outbuf)
        except BrokenPipeError:
            # This happens when they've sent an error and closed; try
            # reading it to figure out what went wrong.
            fut = self.executor.submit(
                cast(CLightningConn, conn).connection.read_message
            )
            try:
                msg = fut.result(1)
            except futures.TimeoutError:
                msg = None
            if msg:
                raise EventError(
                    event, "Connection closed after sending {}".format(msg.hex())
                )
            else:
                raise EventError(event, "Connection closed")

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

    def kill_fundchannel(self) -> None:
        fut = self.fundchannel_future
        self.fundchannel_future = None
        self.is_fundchannel_kill = True

        if fut:
            try:
                fut.result(0)
            except (SpecFileError, futures.TimeoutError):
                pass
            except Exception as ex:
                raise ex from None

    def fundchannel(
        self,
        event: Event,
        conn: Conn,
        amount: int,
        feerate: int = 0,
        expect_fail: bool = False,
    ) -> None:
        # First, check that another fundchannel isn't already running
        if self.fundchannel_future:
            if not self.fundchannel_future.done():
                raise RuntimeError(
                    "{} called fundchannel while another channel funding (fundchannel/init_rbf) is still in process".format(
                        event
                    )
                )
            self.fundchannel_future = None

        def _fundchannel(
            runner: Runner,
            conn: Conn,
            amount: int,
            feerate: int,
            expect_fail: bool = False,
        ) -> str:
            peer_id = conn.pubkey.format().hex()
            # Need to supply feerate here, since regtest cannot estimate fees
            try:
                logging.info(
                    f"fund channel with peer `{peer_id}` with amount `{amount}`"
                )
                return (
                    runner.node.call(
                        "fundchannel",
                        {
                            "node_id": peer_id,
                            "amount": amount,
                            "public": True,
                        },
                    ),
                    True,
                )
            except Exception as ex:
                # FIXME: this should not return None
                # but for now that we do not have any
                # use case where returni value is needed
                # we keep return null.
                #
                # The main reason to do this mess
                # is that in lnprototest do not have
                # any custom way to report a spec violation
                # failure, so for this reason we have different exception
                # at the same time (because this mess is needed to make stuff async
                # and look at exchanged message before finish the call). So
                # the solution is that we log the RPC exception (this may cause a spec
                # validation failure) and we care just the lnprototest exception as
                # real reason to abort.
                return str(ex), False

        def _done(fut: Any) -> None:
            result, ok = fut.result()
            if not ok and not self.is_fundchannel_kill and not expect_fail:
                raise Exception(result)
            logging.info(f"funding channel return `{result}`")
            self.fundchannel_future = None
            self.is_fundchannel_kill = False
            self.cleanup_callbacks.remove(self.kill_fundchannel)

        fut = self.executor.submit(
            _fundchannel, self, conn, amount, feerate, expect_fail
        )
        fut.add_done_callback(_done)
        self.fundchannel_future = fut
        self.cleanup_callbacks.append(self.kill_fundchannel)

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
        return KeySet(
            revocation_base_secret="0000000000000000000000000000000000000000000000000000000000000011",
            payment_base_secret="0000000000000000000000000000000000000000000000000000000000000012",
            delayed_payment_base_secret="0000000000000000000000000000000000000000000000000000000000000013",
            htlc_base_secret="0000000000000000000000000000000000000000000000000000000000000014",
            shachain_seed="FF" * 32,
        )

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
