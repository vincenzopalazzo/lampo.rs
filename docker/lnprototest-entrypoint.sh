#!/bin/bash

echo "running lnprototest"
make
cd tests/lnprototest; poetry lock && poetry install && poetry run make check