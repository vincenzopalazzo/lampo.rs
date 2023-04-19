CC=cargo
FMT=fmt

OPTIONS=

default: fmt
	$(CC) build

fmt:
	$(CC) fmt --all

check:
	$(CC) test --all -- --show-output

clean:
	$(CC) clean

install:
	$(CC) install --path ./lampo-cli
	$(CC) install --path ./lampod-cli
