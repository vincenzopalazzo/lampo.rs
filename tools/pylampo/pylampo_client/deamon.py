import subprocess
import os
import socket
import logging

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
    res = subprocess.run(["lampo-cli", "-u", f"http://127.0.0.1:{port}", "getinfo"], stdout=subprocess.PIPE)
    logging.debug(
        f"lampo-cli -u 'http://127.0.0.1:{port}' getinfo -> {res.returncode} stdout: {res.stdout} stderr: {res.stderr}"
    )
    return res.returncode == 0


def start_lampo(bitcoind: Bitcoind, tmp_file: str, lampod_cli_path = None, conf_lines = None) -> int:
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

    lampod_cli_path = lampod_cli_path if lampod_cli_path is not None else "lampod-cli"

    ## write a file with a bash script
    f = open(f"{lightning_dir}/start.sh", "w")
    f.write(
        f"nohup {lampod_cli_path} --data-dir={lightning_dir} --network=regtest --log-level=debug --api-port={api_port} --log-file={network_dir}/lampod.log &> {network_dir}/daemon.log &"
    )
    if conf_lines is not None:
            f.write(conf_lines)
    f.close()
    ret = subprocess.run(["chmod", "+x", f"{lightning_dir}/start.sh"])
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
