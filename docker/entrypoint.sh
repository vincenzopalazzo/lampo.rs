#!/bin/bash

echo "running lnprototest"
make 
pip3 install poetry
make proto 
