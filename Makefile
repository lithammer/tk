.DEFAULT_GOAL := all
.DELETE_ON_ERROR:
.SUFFIXES:

ZIG ?= zig
ZIGFLAGS ?= --summary all
BIN := zig-out/bin/tk
EMBED := $(wildcard src/store/migrations/*.sql) \
	src/commands/prime.md
SRC := $(shell find src -name '*.zig') \
	build.zig \
	build.zig.zon

all: $(BIN)

$(BIN): $(SRC) $(EMBED)
	$(ZIG) build $(ZIGFLAGS)
	@touch $@

test:
	$(ZIG) build $(ZIGFLAGS) test

run:
	$(ZIG) build $(ZIGFLAGS) run -- $(ARGS)

clean:
	$(RM) -r .zig-cache zig-out

.PHONY: all test run clean
