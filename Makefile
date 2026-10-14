# Wyrdline developer targets. Source ~/kunj-personal/env.sh (or have cargo,
# pnpm, and gitleaks on PATH) before running.
SHELL := /bin/bash
.DEFAULT_GOAL := help
CARGO ?= cargo
PNPM ?= pnpm

.PHONY: help install fmt fmt-check lint typecheck test build deny secrets check \
	fmt-check-rust lint-rust typecheck-rust test-rust build-rust deny-rust \
	fmt-check-web lint-web typecheck-web test-web build-web deny-web \
	schemas schemas-check test-perf test-integration apparmor-install

help: ## List targets
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  %-20s %s\n", $$1, $$2}'

install: ## Install JavaScript dependencies
	$(PNPM) install --frozen-lockfile

fmt: ## Format all code
	$(CARGO) fmt --all
	$(PNPM) format

fmt-check: fmt-check-rust fmt-check-web ## Fail if formatting would change anything
fmt-check-rust:
	$(CARGO) fmt --all -- --check
fmt-check-web:
	$(PNPM) format:check

lint: lint-rust lint-web ## Clippy and ESLint with warnings as errors
lint-rust:
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings
lint-web:
	$(PNPM) lint

typecheck: typecheck-rust typecheck-web ## Type-check every target
typecheck-rust:
	$(CARGO) check --workspace --all-targets --locked
typecheck-web:
	$(PNPM) typecheck

test: test-rust test-web ## Tests that need no privileges
test-rust:
	$(CARGO) nextest run --workspace --locked --no-tests=pass
test-web:
	$(PNPM) test

build: build-rust build-web ## Release build
build-rust:
	$(CARGO) build --workspace --release --locked
build-web:
	$(PNPM) build

deny: deny-rust deny-web ## Licences, advisories, bans, sources
deny-rust:
	$(CARGO) deny check
deny-web:
	$(PNPM) audit --prod --audit-level high

test-perf: ## Throughput and overhead benchmarks (release build, cleaned afterwards)
	$(CARGO) nextest run --workspace --release --locked --run-ignored only -E 'test(/throughput|overhead|latency/)' --no-capture
	$(CARGO) clean --release

test-integration: ## Privileged Cell tests (stage two runs through passwordless sudo)
	WYRD_PRIVILEGED_HELPER=sudo $(CARGO) nextest run --locked -p wyrd-cli --features privileged-tests -E 'test(/^privileged_/)'

WYRD_BIN ?= /usr/local/bin/wyrd
apparmor-install: ## Install the AppArmor profile that allows user namespaces for $(WYRD_BIN) (needs sudo)
	sed 's#/usr/local/bin/wyrd#$(WYRD_BIN)#' deploy/apparmor/wyrd | sudo tee /etc/apparmor.d/wyrd >/dev/null
	sudo apparmor_parser -r /etc/apparmor.d/wyrd

secrets: ## Secret scan of the working tree and history
	gitleaks detect --no-banner --redact

check: fmt-check lint typecheck test deny secrets ## Local gate (subset available so far)

schemas: ## Regenerate TypeScript types and JSON Schema from wyrd-schema
	$(CARGO) run --quiet --locked -p wyrd-schema --bin wyrd-gen-schemas -- .

schemas-check: schemas ## Fail if generated schema files are stale
	@git add -N packages/schemas docs/schemas >/dev/null 2>&1 || true
	@git diff --exit-code --stat -- packages/schemas docs/schemas || { echo "Generated schemas are stale: run 'make schemas' and commit."; exit 1; }
