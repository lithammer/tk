.DEFAULT_GOAL := all
.DELETE_ON_ERROR:
.SUFFIXES:

all:
	cargo build --bin tk

test:
	cargo test

lint:
	cargo clippy --all-targets

run:
	cargo run --bin tk -- $(ARGS)

release:
	cargo build --release --bin tk

clean:
	cargo clean

manpage:
	cargo run --locked --example manpage

check-manpage:
	cargo run --locked --example manpage -- --check

hooks:
	git config core.hooksPath .githooks

.PHONY: all test lint run release clean manpage check-manpage hooks
