#!/bin/bash

set -e

# Once without VLS, so the default build never depends on it, then the
# VLS-backed tests against the vlsd built into the image.
make check TEST_LOG_LEVEL='debug'
make check-vls TEST_LOG_LEVEL='debug'