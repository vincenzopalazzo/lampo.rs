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

# Regtest integration tests. Needs bitcoind on PATH. The rbitcoin lane
# also needs rbitcoin-node, or LAMPO_RBITCOIN pointing at it.
# LAMPO_CHAIN=core (default) runs Core only. LAMPO_CHAIN=all runs both.
LAMPO_CHAIN ?= core
PLUGIN_BIN=$(CURDIR)/target/debug/examples/lampo-bitcoind

integration: plugin
	LAMPO_BITCOIND=$(PLUGIN_BIN) LAMPO_CHAIN_BACKEND=core \
		TEST_LOG_LEVEL=$(TEST_LOG_LEVEL) $(CC) test -p tests --lib $(ARGS)
	@if [ "$(LAMPO_CHAIN)" = "all" ]; then \
		LAMPO_BITCOIND=$(PLUGIN_BIN) LAMPO_CHAIN_BACKEND=rbitcoin \
			TEST_LOG_LEVEL=$(TEST_LOG_LEVEL) $(CC) test -p tests --lib $(ARGS); \
	fi

plugin:
	$(CC) build -p lampo-plugin-sdk --example lampo-bitcoind

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

