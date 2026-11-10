.DEFAULT_GOAL := help
CARGO ?= cargo

.PHONY: help build run release test check fmt lint clean install config

help: ## Show available targets
	@awk 'BEGIN { FS = ":.*## " } /^[a-zA-Z_-]+:.*## / { printf "  %-12s %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

build: ## Build the debug binary
	$(CARGO) build

run: build ## Build and launch rst
	./target/debug/rst

release: ## Build an optimized binary
	$(CARGO) build --release

test: ## Run unit tests locally (no GitHub Actions)
	$(CARGO) test

check: ## Check code locally
	$(CARGO) check

fmt: ## Format Rust source
	$(CARGO) fmt --all

lint: ## Run local Clippy
	$(CARGO) clippy --all-targets -- -D warnings

clean: ## Remove local build artifacts
	$(CARGO) clean

install: release ## Install rst in ~/.local/bin
	@mkdir -p "$(HOME)/.local/bin"
	install -m 755 ./target/release/rst "$(HOME)/.local/bin/rst"

config: ## Copy example configuration (do not overwrite)
	@mkdir -p "$(HOME)/.config/rst"
	@test -e "$(HOME)/.config/rst/config.toml" || cp config/default.toml "$(HOME)/.config/rst/config.toml"
	@echo "Config: $(HOME)/.config/rst/config.toml"
