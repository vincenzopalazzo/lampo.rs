import pytest

from typing import Any
from threading import Thread

from lampo_py import LampoDeamon


@pytest.fixture()  # type: ignore
def node(pytestconfig: Any) -> Any:
    lampo = LampoDeamon(b"/home/vincent/.lampo/testnet/")
    thread = Thread(target=lampo.listen)
    thread.daemon = True
    thread.start()

    yield lampo

    thread.join(1)
