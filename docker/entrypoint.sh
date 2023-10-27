#!/bin/bash

echo "lnprototest enabled: $PROTO_TEST"
if [ "$PROTO_TEST" -eq 1 ]; then
    echo "running lnprototest"
    make 
    cp target/debug/liblampo.so /usr/local/lib
    pip3 install poetry
    cd tests/lnprototest; poetry install && poetry run make check 
else
    echo "running integration tests"
    RUST_BACKTRACE=full make integration
fi
