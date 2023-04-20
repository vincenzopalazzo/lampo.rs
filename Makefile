CC=cargo
FMT=fmt

OPTIONS=

default: fmt
	rustup run nightly $(CC) build

fmt:
	rustup run nightly $(CC) fmt --all

check:
	rustup run nightly $(CC) test --all -- --show-output

clean:
	rustup run nightly $(CC) clean

install:
	$(CC) install --path ./lampo-cli
	rustup run nightly $(CC) install --path ./lampod-cli
