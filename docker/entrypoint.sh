#!/bin/bash

if [[ -z "${PROTO_TEST}" ]]; then
    pip3 install poetry
    cd tests/lnprototest; poetry install && poetry run "make check" 
else
    RUST_BACKTRACE=full make integration
fi
