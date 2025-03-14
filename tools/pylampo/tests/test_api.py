import subprocess
import os
import time
import socket
import tempfile
import logging

import pytest

logging.basicConfig(level=logging.INFO)

from contextlib import closing

from lnprototest.backend import Bitcoind
from lnprototest.utils import wait_for

from pylampo_client.client import LampoClient


def reserve_port() -> int:
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


def start_bitcoind(tmp_dir):
    """Start the bitcoind daemon in regtest mode."""
    bitcoind = Bitcoind(tmp_dir)
    bitcoind.start()
    return bitcoind


def lampocli_check(port: int) -> bool:
    res = subprocess.run(["lampo-cli", "getinfo"], stdout=subprocess.PIPE)
    logging.debug(
        f"lampo-cli -u http://127.0.0.1:{port} getinfo -> {res.returncode} stdout: {res.stdout} stderr: {res.stderr}"
    )
    return res.returncode == 0


def start_lampo(bitcoind: Bitcoind, tmp_file) -> int:
    lightning_dir = os.path.join(tmp_file, "lampo")
    if not os.path.exists(lightning_dir):
        os.makedirs(lightning_dir)
    network_dir = os.path.join(lightning_dir, "regtest")
    if not os.path.exists(network_dir):
        os.makedirs(network_dir)
    lightning_port = reserve_port()  # get a random one
    f = open(f"{network_dir}/lampo.conf", "w")
    f.write(f"port={lightning_port}\n")
    # configure bitcoin core
    f.write(
        f"backend=core\ncore-user=rpcuser\ncore-pass=rpcpass\nnetwork=regtest\ncore-url=http://127.0.0.1:{bitcoind.port}\n"
    )
    f.flush()
    f.close()
    api_port = reserve_port()

    ## write a file with a bash script
    f = open(f"{lightning_dir}/start.sh", "w")
    f.write(
        f"nohup lampod-cli --data-dir={lightning_dir} --network=regtest --log-level=debug --api-port={api_port} --log-file={network_dir}/lampod.log &> {network_dir}/daemon.log &"
    )
    f.close()
    ret = subprocess.run(["chmod", "+x", f"{lightning_dir}/start.sh"])
    logging.info(f"ret: {ret}")
    ret = subprocess.run(
        [
            "cp",
            "/Users/vincenzopalazzo/.lampo/signet/wallet.dat",
            f"{network_dir}/wallet.dat",
        ]
    )
    logging.info(f"ret: {ret}")
    # run lampod-cli deamon with the --data-dir = lampo_dir and --network=regtest
    ret = subprocess.Popen(
        [
            "sh",
            f"{lightning_dir}/start.sh",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    stdout, stderr = ret.communicate()
    logging.info(f"stdout: {stdout} stderr: {stderr}")
    logging.info(f"{open(f'{network_dir}/daemon.log').read()}")
    return api_port


def getinfo(api_port: int):
    """Call the getinfo method from the client.py."""
    lampo = LampoClient(f"http://127.0.0.1:{api_port}")
    response = lampo.call("getinfo", {})
    return response.json()


def try_getinfo(api_port: int) -> bool:
    try:
        _ = getinfo(api_port)
        return True
    except Exception as e:
        logging.error(f"Error calling getinfo: {e}")
        return False


@pytest.fixture
def setup_environment():
    """Setup the environment by starting the necessary daemons."""
    directory = tempfile.mkdtemp(prefix="lnpt-cl-")
    bitcoind = start_bitcoind(directory)
    logging.info(bitcoind.rpc.getblockchaininfo())
    api_port = start_lampo(bitcoind, directory)
    yield directory, api_port, bitcoind
    # print lampo logs
    lightning_dir = os.path.join(directory, "lampo")
    network_dir = os.path.join(lightning_dir, "regtest")
    with open(f"{network_dir}/lampod.log", "r") as log_file:
        logs = log_file.read()
        logging.debug("Lampo logs:\n%s", logs)

    with open(f"{network_dir}/daemon.log", "r") as log_file:
        logs = log_file.read()
        logging.debug("Daemon logs:\n%s", logs)


def test_init_lampo(setup_environment):
    """Test the getinfo method."""
    directory, api_port, bitcoind = setup_environment

    # check if the lampo-cli is working
    wait_for(lambda: lampocli_check(api_port) == True, timeout=60)

    # check if the http api works
    wait_for(lambda: try_getinfo(api_port) == True, timeout=10)
