import pytest
import importlib

import lnprototest
import pyln.spec.bolt1
import pyln.spec.bolt2
import pyln.spec.bolt7

from typing import Any, Callable, Generator, List
from threading import Thread

from lampo_lnprototest import LampoRunner
from pyln.proto.message import MessageNamespace


@pytest.fixture()  # type: ignore
def runner(pytestconfig: Any) -> Any:
    runner = LampoRunner(pytestconfig)
    thread = Thread(target=runner.listen)
    thread.daemon = True
    thread.start()

    yield runner

    runner.teardown()
    thread.join(1)


@pytest.fixture()
def namespaceoverride(
    pytestconfig: Any,
) -> Generator[Callable[[MessageNamespace], None], None, None]:
    """Use this if you want to override the message namespace"""

    def _setter(newns: MessageNamespace) -> None:
        lnprototest.assign_namespace(newns)

    yield _setter
    # Restore it
    lnprototest.assign_namespace(lnprototest.peer_message_namespace())


@pytest.fixture()
def with_proposal(
    pytestconfig: Any,
) -> Generator[Callable[[List[str]], None], None, None]:
    """Use this to add additional messages to the namespace
    Useful for testing proposed (but not yet merged) spec mods.  Noop if it seems already merged.
    """

    def _setter(proposal_csv: List[str]) -> None:
        # Testing first line is cheap, pretty effective.
        if proposal_csv[0] not in (
            pyln.spec.bolt1.csv + pyln.spec.bolt2.csv + pyln.spec.bolt7.csv
        ):
            # We merge *csv*, because then you can add tlv entries; merging
            # namespaces with duplicate TLVs complains of a clash.
            lnprototest.assign_namespace(
                lnprototest.make_namespace(
                    pyln.spec.bolt1.csv
                    + pyln.spec.bolt2.csv
                    + pyln.spec.bolt7.csv
                    + proposal_csv
                )
            )

    yield _setter

    # Restore it
    lnprototest.assign_namespace(lnprototest.peer_message_namespace())
