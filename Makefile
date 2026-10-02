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
	$(CC) test --workspace --exclude lampo-vls -- --show-output

# Same suite pieces with the Validating Lightning Signer backend: the
# lampo-vls unit tests and the VLS-backed node tests, which need
# VLSD_EXE and REMOTE_HSMD_SOCKET_EXE.
check-vls:
	$(CC) test -p lampo-vls
	$(CC) test -p tests --features vls lampo_vls_tests -- --show-output --test-threads=1

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
