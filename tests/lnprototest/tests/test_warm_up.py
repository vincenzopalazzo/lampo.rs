import pytest
import logging
import traceback

from typing import Any, Union, Sequence, List

from pyln.spec import bolt1
from lnprototest import SpecFileError, TryAll
from lnprototest.runner import Runner
from lnprototest.event import Msg, Disconnect, Connect, Event, ExpectMsg


# FIXME: move this in a util module of lnprototest
def get_traceback(e: Exception) -> str:
    lines = traceback.format_exception(type(e), e, e.__traceback__)
    return "".join(lines)


def run_runner(runner: Runner, test: Union[Sequence, List[Event], Event]) -> None:
    """
    The pytest using the assertion as safe failure, and the exception it is only
    an event that must not happen.

    From design, lnprototest fails with an exception, and for this reason, if the
    lnprototest throws an exception, we catch it, and we fail with an assent.
    """
    try:
        runner.run(test)
    except Exception as ex:
        runner.stop(print_logs=True)
        logging.error(get_traceback(ex))
        assert False, ex


def test_namespace_override(runner: Runner, namespaceoverride: Any) -> None:
    # Truncate the namespace to just BOLT1
    namespaceoverride(bolt1.namespace)

    # Try to send a message that's not in BOLT1
    with pytest.raises(SpecFileError, match=r"Unknown msgtype open_channel"):
        Msg("open_channel")


@pytest.mark.skip("The connection event get stuck for some reason")
def test_on_simple_init(runner: Runner, namespaceoverride: Any) -> None:
    """ "
    Send from the runner to ldk a fist `init` connection
    as specified in the BOL1
    """
    namespaceoverride(bolt1.namespace)
    test = [
        Connect(connprivkey="03"),
        ExpectMsg("init"),
        Msg("init", globalfeatures="", features=""),
        # optionally disconnect that first one
        TryAll([], Disconnect()),
        Connect(connprivkey="02"),
        # You should always handle us echoing your own features back!
        ExpectMsg("init"),
        Msg("init", globalfeatures="", features=""),
    ]

    run_runner(runner, test)
