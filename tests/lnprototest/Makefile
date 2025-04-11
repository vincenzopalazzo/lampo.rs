CC=python3
FMT=black
PYARGS="--log-cli-level=debug"

default: fmt

fmt:
	$(FMT) .

check:
	pytest . $(PYARGS)
