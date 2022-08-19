CC=cargo
FMT=fmt

OPTIONS=

default: fmt
	$(CC) build

fmt:
	$(CC) fmt --all

check:
	$(CC) test --all

clean:
	$(CC) clean
