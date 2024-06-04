#!/bin/bash

echo "running lnprototest"
bitcoind --version
make 
pip3 install poetry
make proto 
