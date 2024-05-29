import pytest

from typing import Any
from threading import Thread

from lampo_py import LampoDaemon


@pytest.fixture()  # type: ignore
def node(pytestconfig: Any) -> Any:
    lampo = LampoDaemon("/home/vincent/.lampo/testnet")
    thread = Thread(target=lampo.listen)
    thread.daemon = True
    thread.start()

    yield lampo

    thread.join(1)
