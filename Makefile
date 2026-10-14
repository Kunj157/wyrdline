# Wyrdline developer targets. Source ~/kunj-personal/env.sh (or have cargo,
# pnpm, and gitleaks on PATH) before running.
SHELL := /bin/bash
.DEFAULT_GOAL := help
CARGO ?= cargo

.PHONY: help fmt fmt-check lint typecheck test build deny secrets check

help: ## List targets
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  %-20s %s\n", $$1, $$2}'

fmt: ## Format all code
	$(CARGO) fmt --all

fmt-check: ## Fail if formatting would change anything
	$(CARGO) fmt --all -- --check

lint: ## Clippy with warnings as errors
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings

typecheck: ## Type-check every target
	$(CARGO) check --workspace --all-targets --locked

test: ## Unit and integration tests that need no privileges
	$(CARGO) nextest run --workspace --locked --no-tests=pass

build: ## Release build
	$(CARGO) build --workspace --release --locked

deny: ## Licences, advisories, bans, sources
	$(CARGO) deny check

secrets: ## Secret scan of the working tree and history
	gitleaks detect --no-banner --redact

check: fmt-check lint typecheck test deny secrets ## Local gate (subset available so far)
