.DEFAULT_GOAL := all
.DELETE_ON_ERROR:
.SUFFIXES:

all:
	cargo build

test:
	cargo test

lint:
	cargo clippy

run:
	cargo run -- $(ARGS)

release:
	cargo build --release

clean:
	cargo clean

manpage:
	cargo run --locked --example manpage

check-manpage:
	cargo run --locked --example manpage -- --check

hooks:
	git config core.hooksPath .githooks

.PHONY: all test lint run release clean manpage check-manpage hooks
