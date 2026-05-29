.DEFAULT_GOAL := all
.DELETE_ON_ERROR:
.SUFFIXES:

all:
	cargo build

test:
	cargo test

run:
	cargo run -- $(ARGS)

release:
	cargo build --release

clean:
	cargo clean

.PHONY: all test run release clean
