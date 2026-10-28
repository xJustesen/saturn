# Build and install saturn.
#
#   make            build a release binary into target/release/saturn
#   make install    build and copy it to $(PREFIX)/bin (default ~/.local/bin)
#   make uninstall  remove the installed binary
#
# Override the destination with PREFIX, e.g. `make install PREFIX=/usr/local`.

CARGO   ?= cargo
PREFIX  ?= $(HOME)/.local
BINDIR  ?= $(PREFIX)/bin
INSTALL ?= install

BIN     := saturn
TARGET  := target/release/$(BIN)

.PHONY: all build install uninstall test clean

all: build

build:
	$(CARGO) build --release

install: build
	$(INSTALL) -d "$(BINDIR)"
	$(INSTALL) -m 755 "$(TARGET)" "$(BINDIR)/$(BIN)"
	@echo "Installed $(BINDIR)/$(BIN)"

uninstall:
	rm -f "$(BINDIR)/$(BIN)"

test:
	$(CARGO) test

clean:
	$(CARGO) clean
