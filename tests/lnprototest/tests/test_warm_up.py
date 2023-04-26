import pytest

from typing import Any

from pyln.spec import bolt1
from lnprototest import SpecFileError
from lnprototest.runner import Runner
from lnprototest.event import Msg


def test_namespace_override(runner: Runner, namespaceoverride: Any) -> None:
    # Truncate the namespace to just BOLT1
    namespaceoverride(bolt1.namespace)

    # Try to send a message that's not in BOLT1
    with pytest.raises(SpecFileError, match=r"Unknown msgtype open_channel"):
        Msg("open_channel")
