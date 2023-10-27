CC=python3
FMT=black
PYARGS="--log-cli-level=DEBUG"

default: fmt

fmt:
	$(FMT) .

check:
	pytest . $(PYARGS)
