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

integration: default
	 TEST_LOG_LEVEL=$(TEST_LOG_LEVEL) $(CC) test -p tests $(ARGS)

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

