__version__ = "0.1.0"
import os

if os.getenv('USE_LNPROTOTEST_DEPENDENCIES', 'false').lower() == 'true':
    from .deamon import start_bitcoind, lampocli_check, start_lampo, reserve_port, wait_for

from .client import LampoClient