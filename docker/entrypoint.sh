#!/bin/bash

echo "running lnprototest"
make 
pip3 install poetry
cd tests/lnprototest; poetry install && poetry run make check 
