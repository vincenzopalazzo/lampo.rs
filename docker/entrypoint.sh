#!/bin/bash

echo "running lnprototest"
make
pip3 install poetry --break-system-packages
cd tests/lnprototest; poetry lock && poetry install && poetry run make check PYARGS='--log-level=trace'
