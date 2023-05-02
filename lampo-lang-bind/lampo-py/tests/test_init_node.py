from threading import Thread
from lampo_py import LampoDeamon


def test_init_node(node):
    result = node.call("getinfo", {})
    print(f"{result}")
    assert result is not None
    assert "node_id" in result, f"The result has no `node_id`: {result}"
