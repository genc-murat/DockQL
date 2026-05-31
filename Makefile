# DOL — Docker Observability Language
#
# Usage:
#   make          — build (debug)
#   make release  — build (release)
#   make install  — build release + install binary
#   make test     — run all tests
#   make check    — cargo check
#   make lint     — clippy + fmt checks
#   make clean    — cargo clean
#   make doc      — build API docs
#   make run      — cargo run

BINARY_NAME      := dol
INSTALL_DIR      ?= $(HOME)/.cargo/bin

.PHONY: all build release install uninstall test check lint clean doc run help

all: build

build:
	cargo build

release:
	cargo build --release

install: release
	@mkdir -p "$(INSTALL_DIR)"
	cp "target/release/$(BINARY_NAME)" "$(INSTALL_DIR)/$(BINARY_NAME)"
	@echo "✅ $(BINARY_NAME) installed to $(INSTALL_DIR)/$(BINARY_NAME)"
	@echo "   Make sure $(INSTALL_DIR) is in your PATH."

uninstall:
	rm -f "$(INSTALL_DIR)/$(BINARY_NAME)"
	@echo "✅ $(BINARY_NAME) removed from $(INSTALL_DIR)"

test:
	cargo test

check:
	cargo check

lint:
	cargo clippy --all-targets -- -D warnings
	cargo fmt --check

clean:
	cargo clean

doc:
	cargo doc --no-deps --all-features

run:
	cargo run -- $(ARGS)

help:
	@echo "DOL — Docker Observability Language"
	@echo ""
	@echo "Targets:"
	@echo "  make          — build (debug)"
	@echo "  make release  — build (release)"
	@echo "  make install  — build release + install binary"
	@echo "  make uninstall — remove installed binary"
	@echo "  make test     — run all tests"
	@echo "  make check    — cargo check"
	@echo "  make lint     — clippy + fmt checks"
	@echo "  make clean    — cargo clean"
	@echo "  make doc      — build API docs"
	@echo "  make run      — cargo run (pass ARGS=... for CLI flags)"
	@echo ""
	@echo "Variables:"
	@echo "  INSTALL_DIR  — install target directory (default: ~/.cargo/bin)"
	@echo "  ARGS         — arguments passed to 'cargo run -- $$ARGS'"
