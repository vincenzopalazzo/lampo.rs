CC=cargo
FMT=fmt

ARGS=

default: fmt
	rustup run nightly $(CC) build

fmt:
	rustup run nightly $(CC) fmt --all

check:
	rustup run nightly $(CC) test --all -- --show-output

clean:
	rustup run nightly $(CC) clean

install:
	$(CC) build
	$(CC) install --path ./lampo-cli
	$(CC) install --path ./lampod-cli
	sudo cp target/debug/liblampo_lib.so /usr/local/lib

integration: default
	$(CC) test -p tests $(ARGS)
