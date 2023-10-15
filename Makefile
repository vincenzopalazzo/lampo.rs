CC=cargo
FMT=fmt

ARGS=

default: fmt
	$(CC) build

fmt:
	$(CC) fmt --all

check:
	$(CC) test --all -- --show-output

clean:
	$(CC) clean

install:
	$(CC) build
	$(CC) install --locked --path ./lampo-cli 
	$(CC) install --locked --path ./lampod-cli --debug
	sudo cp target/debug/liblampo.so /usr/local/lib

integration: default
	$(CC) test -p tests $(ARGS)
