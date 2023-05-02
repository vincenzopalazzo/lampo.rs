from cffi import FFI

ffi = FFI()

ffi.cdef(
    """
    typedef struct LampoDeamon LampoDeamon;

    /**
    * Add a JSON RPC 2.0 Sever that listen on a unixsocket, and return a error code
    * < 0 is an error happens, or 0 is all goes well.
    */
    int64_t add_jsonrpc_on_unixsocket(struct LampoDeamon *lampod);

    /**
    * Allow to create a lampo deamon from a configuration patch!
    */
    void free_lampod(struct LampoDeamon *lampod);

    const char *lampod_call(struct LampoDeamon *lampod, const char *method, const char *buffer);

    /**
    * Allow to create a lampo deamon from a configuration patch!
    */
    struct LampoDeamon *new_lampod(const char *conf_path);

    void lampo_listen(struct LampoDeamon *lampod);
"""
)

lampod = ffi.dlopen("/usr/local/lib/liblampo_lib.so")

import json
import logging
from typing import Dict, Any


class LampoDeamon:
    """
    Python Wrapper around the Lampo Lightning Network Node

    Based on https://bheisler.github.io/post/calling-rust-in-python
    """

    def __init__(self, home_path: str) -> None:
        # FIXME: add the way to create the dir inside the lampod
        self.__inner = lampod.new_lampod(home_path)

    def listen(self):
        """ ""
        Run The lightning node!
        """
        lampod.add_jsonrpc_on_unixsocket(self.__inner)
        lampod.lampo_listen(self.__inner)

    def call(self, method: str, payload: Dict[str, Any]) -> Dict[str, Any]:
        """ " Perform a call to the lightning node"""
        result = lampod.lampod_call(self.__inner, bytes(method), b"{}")
        logging.debug(f"raw data {result}")
        result = ffi.string(result).decode("utf-8")
        assert result is not None
        result = json.loads(result)

        logging.debug(f"call to `{method}` return {result}")
        return result

    def __exit__(self, exc_type, exc_value, traceback):
        lampod.free_lampod(self.__inner)
        self.__inner = None
