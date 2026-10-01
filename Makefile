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

# Zig 0.16 plugin SDK and the bitcoind chain backend.
# The binary name matches what lampod-cli looks up next to itself.
zig-sdk:
	cd plugin-sdk/zig && zig build test

zig-bitcoind:
	cd lampo-bitcoind/zig && zig build -Doptimize=ReleaseSafe

