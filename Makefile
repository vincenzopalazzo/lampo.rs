CC=cargo
FMT=fmt

ARGS=
TEST_LOG_LEVEL=

default: fmt
	$(CC) build

fmt:
	$(CC) fmt --all

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

proto:
	cd tests/lnprototest; poetry install; poetry run make check