.DEFAULT_GOAL := help
CARGO ?= cargo
UNAME_S := $(shell uname -s)

.PHONY: help build run run-cli release test check fmt lint clean install config macos-app macos-release install-macos shell-integration

help: ## Show available targets
	@awk 'BEGIN { FS = ":.*## " } /^[a-zA-Z_-]+:.*## / { printf "  %-15s %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

build: ## Build the debug binary
	$(CARGO) build

ifeq ($(UNAME_S),Darwin)
run: macos-app ## Build and launch the native macOS app with Dock icon
	open -n "$(CURDIR)/target/debug/rst Debug.app"
else
run: build ## Build and run the Linux/Wayland binary
	./target/debug/rst
endif

run-cli: build ## Run the binary directly, keeping console logs attached
	./target/debug/rst

release: ## Build an optimized binary
	$(CARGO) build --release

macos-app: build ## Package target/debug/rst Debug.app with icon (macOS only)
	sh ./scripts/package-macos.sh debug

macos-release: release ## Package target/release/rst Preview.app with icon (macOS only)
	sh ./scripts/package-macos.sh release

install-macos: macos-release ## Install the native app into ~/Applications (macOS)
	@mkdir -p "$(HOME)/Applications"
	@lsregister="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"; \
	  if [ -x "$$lsregister" ] && [ -d "$(HOME)/Applications/rst.app" ]; then "$$lsregister" -u "$(HOME)/Applications/rst.app" >/dev/null 2>&1 || true; fi
	@rm -rf "$(HOME)/Applications/rst.app"
	cp -R "./target/release/rst Preview.app" "$(HOME)/Applications/rst.app"
	@sh ./scripts/macos-bundle-identity.sh "$(HOME)/Applications/rst.app" installed

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

install: release ## Install the CLI executable in ~/.local/bin
	@mkdir -p "$(HOME)/.local/bin"
	install -m 755 ./target/release/rst "$(HOME)/.local/bin/rst"

shell-integration: build ## Install OSC 133 hooks for bash, zsh and fish (one-time, idempotent)
	./target/debug/rst --install-shell-integration

config: ## Copy example configuration (do not overwrite)
	@mkdir -p "$(HOME)/.config/rst"
	@test -e "$(HOME)/.config/rst/config.toml" || cp config/default.toml "$(HOME)/.config/rst/config.toml"
	@echo "Config: $(HOME)/.config/rst/config.toml"

.PHONY: stress benchmark benchmark-parser
stress: ## Run extended deterministic robustness tests (512,000 random operations)
	RST_STRESS_SEEDS=256 RST_STRESS_STEPS=2000 $(CARGO) test --release robustness_tests

benchmark: release ## Measure real-window startup, idle CPU and PTY latency (Python 3)
	python3 scripts/benchmark.py

benchmark-parser: ## Measure parser throughput without a window
	$(CARGO) test --release parser_throughput_benchmark -- --ignored --nocapture

.PHONY: benchmark-startup
benchmark-startup: release ## Compare startup with Alacritty (Python 3, native desktop)
	python3 scripts/benchmark-startup.py

.PHONY: test-macos-packaging
test-macos-packaging: release ## Check bundle identities and local signatures (macOS, Python 3)
	python3 scripts/test-macos-packaging.py
