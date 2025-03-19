import subprocess
import os
import time
import socket
import tempfile
import logging

import pytest

logging.basicConfig(level=logging.INFO)

from pylampo_client.client import LampoClient
from pylampo_client.deamon import start_bitcoind, lampocli_check, start_lampo, wait_for

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
