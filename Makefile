CC=cargo
FMT=fmt

ARGS=
TEST_LOG_LEVEL=

default: fmt
	$(CC) build

fmt:
	$(CC) fmt --all
	# $(CC) clippy --workspace

check:
	$(CC) test --all -- --show-output

clean:
	$(CC) clean

install:
	$(CC) build --release
	$(CC) install --locked --path ./lampo-cli
	$(CC) install --locked --path ./lampod-cli

# Test binaries land in target/ci-bins. `unit` and `integration` rerun
# those binaries and do not call cargo again.
LAMPO_CHAIN ?= core

bins:
	./tools/test-bins.sh build

unit: bins
	TEST_LOG_LEVEL=$(TEST_LOG_LEVEL) ./tools/test-bins.sh run-unit

integration: bins
	LAMPO_CHAIN_BACKEND=core TEST_LOG_LEVEL=$(TEST_LOG_LEVEL) \
		./tools/test-bins.sh run-integration
	@if [ "$(LAMPO_CHAIN)" = "all" ]; then \
		LAMPO_CHAIN_BACKEND=rbitcoin TEST_LOG_LEVEL=$(TEST_LOG_LEVEL) \
			./tools/test-bins.sh run-integration; \
	fi

audit:
	$(CC) install cargo-audit
	$(CC) audit

# Zig plugin SDK. Needs the pin in plugin-sdk/zig/.zigversion, not brew zig.
# The binary name matches what lampod-cli looks up next to itself.
ZIG ?= zig
zig-sdk:
	cd plugin-sdk/zig && $(ZIG) build test

zig-bitcoind:
	cd lampo-bitcoind/zig && $(ZIG) build -Doptimize=ReleaseSafe

