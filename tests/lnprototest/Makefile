CC=python3
FMT=black
PYARGS=

default: fmt

fmt:
	$(FMT) .

check:
	pytest . --log-cli-level=DEBUG $(PYARGS)