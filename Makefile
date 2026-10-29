# Wyrdline developer targets. Source ~/kunj-personal/env.sh (or have cargo,
# pnpm, and gitleaks on PATH) before running.
SHELL := /bin/bash
.DEFAULT_GOAL := help
CARGO ?= cargo
PNPM ?= pnpm

.PHONY: help demo test-evals install fmt fmt-check lint typecheck test build deny secrets check \
	fmt-check-rust lint-rust typecheck-rust test-rust build-rust deny-rust \
	fmt-check-web lint-web typecheck-web test-web build-web deny-web \
	schemas schemas-check test-perf test-integration test-scenarios apparmor-install policy-fixtures policies test-policies test-escape \
	test-e2e test-e2e-webgpu test-e2e-hub e2e-build site test-site test-hub fuzz fuzz-seeds

help: ## List targets
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  %-20s %s\n", $$1, $$2}'

install: ## Install JavaScript dependencies
	$(PNPM) install --frozen-lockfile

fmt: ## Format all code
	$(CARGO) fmt --all
	$(CARGO) fmt --all --manifest-path policies/Cargo.toml
	$(PNPM) format

fmt-check: fmt-check-rust fmt-check-web ## Fail if formatting would change anything
fmt-check-rust:
	$(CARGO) fmt --all -- --check
	$(CARGO) fmt --all --manifest-path policies/Cargo.toml -- --check
fmt-check-web:
	$(PNPM) format:check

lint: lint-rust lint-web ## Clippy and ESLint with warnings as errors
lint-rust:
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings
	$(CARGO) clippy --workspace --locked --manifest-path policies/Cargo.toml --target wasm32-wasip2 -- -D warnings
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

test-perf: ## Throughput, overhead and latency measurements (release build, needs sudo; cleaned afterwards)
	s=0; $(CARGO) nextest run --workspace --release --locked --features wyrd-sensord/privileged-tests \
		--run-ignored only -E 'test(/throughput|overhead|latency/)' --no-capture --test-threads 1 --no-fail-fast || s=$$?; \
		$(CARGO) clean --release; exit $$s

e2e-build:
	$(CARGO) build --locked -p wyrdd --bin wyrdd --example e2e_fixture
	$(CARGO) build --locked -p wyrd-cli
	scripts/build-studio-wasm.sh
	$(PNPM) --filter @wyrdline/studio build

test-e2e: e2e-build ## Studio end to end: Playwright (headless Chromium) against a real wyrdd
	$(PNPM) exec playwright test -c tests/e2e --project=chromium

test-e2e-webgpu: e2e-build ## The WebGPU river, headed Chromium with SwiftShader on a virtual display
	WYRD_E2E_WEBGPU=1 xvfb-run -a $(PNPM) exec playwright test -c tests/e2e --project=webgpu

test-e2e-hub: e2e-build ## Studio against a real Hub and wyrdd (needs Postgres, MinIO and the WYRD_E2E_HUB_* variables, see tests/e2e/hub)
	$(PNPM) exec playwright test -c tests/e2e/playwright.hub.config.ts

site: ## Landing page with Studio and the demo session, as published (apps/site/dist)
	scripts/build-site.sh

test-site: site ## Landing page end to end: Playwright against the published build
	$(PNPM) exec playwright test -c tests/site

test-integration: ## Privileged Cell, scenario, and sensor tests (through passwordless sudo)
	WYRD_PRIVILEGED_HELPER=sudo $(CARGO) nextest run --locked -p wyrd-cli -p wyrd-fakeagent -p wyrd-sensord -p wyrdd \
		--features wyrd-cli/privileged-tests,wyrd-fakeagent/privileged-tests,wyrd-sensord/privileged-tests,wyrdd/privileged-tests \
		-E 'test(/^privileged_/)'

test-escape: ## The escape suite: an agent in a Cell tries to get around every enforcement layer (sudo)
	WYRD_PRIVILEGED_HELPER=sudo $(CARGO) nextest run --locked -p wyrd-cli --features wyrd-cli/privileged-tests --test escape

test-hub: ## Hub lint and tests with analytics (DuckDB), Postgres 17 and MinIO in containers (needs Docker)
	$(CARGO) clippy --locked -p wyrd-hub --all-targets --features wyrd-hub/container-tests,wyrd-hub/analytics -- -D warnings
	$(CARGO) nextest run --locked -p wyrd-hub --features wyrd-hub/container-tests,wyrd-hub/analytics

demo: ## The plan §2 demo with the scripted agent: victim shop, Cell, sensor, default policy (sudo)
	WYRD_PRIVILEGED_HELPER=sudo $(CARGO) nextest run --locked -p wyrd-cli --features wyrd-cli/privileged-tests --test demo --no-capture

test-evals: ## Divergence and claim evals against the committed baseline (recorded LLM responses; EVAL_LIVE=1 records new ones)
	$(CARGO) run --quiet --locked -p wyrd-divergence --bin wyrd-evals -- $(EVAL_ARGS)

test-scenarios: ## Fakeagent-driven full-pipeline scenarios (unprivileged; Cell ones run in test-integration)
	$(CARGO) build --locked -p wyrd-cli -p wyrd-fakeagent
	$(CARGO) nextest run --locked -p wyrd-fakeagent --test scenarios

WYRD_BIN ?= /usr/local/bin/wyrd
apparmor-install: ## Install the AppArmor profile that allows user namespaces for $(WYRD_BIN) (needs sudo)
	sed 's#/usr/local/bin/wyrd#$(WYRD_BIN)#' deploy/apparmor/wyrd | sudo tee /etc/apparmor.d/wyrd >/dev/null
	sudo apparmor_parser -r /etc/apparmor.d/wyrd

sensord-install: ## Build and install wyrd-sensord as a systemd service (needs sudo; system change)
	$(CARGO) build --release --locked -p wyrd-sensord
	sudo install -m 0755 target/release/wyrd-sensord /usr/local/bin/wyrd-sensord
	sudo install -m 0644 deploy/systemd/wyrd-sensord.service /etc/systemd/system/wyrd-sensord.service
	sudo systemctl daemon-reload
	sudo systemctl enable --now wyrd-sensord.service

policies: ## Build the shipped policies (policies/default.wasm, policies/strict.wasm)
	$(CARGO) build --quiet --locked -p wyrd-cli
	target/debug/wyrd policy build policies/default
	target/debug/wyrd policy build policies/strict

test-policies: policies ## Policy unit tests (native) and conformance cases against the built components
	$(CARGO) test --quiet --locked --manifest-path policies/Cargo.toml -p policy-common
	target/debug/wyrd policy test policies/default.wasm policies/default/cases.toml
	target/debug/wyrd policy test policies/strict.wasm policies/strict/cases.toml

policy-fixtures: ## Rebuild the committed test policy components in policies/fixtures/
	scripts/build-policy-fixtures.sh

studio-wasm: ## Build the WebAssembly correlator into the Studio (offline mode)
	scripts/build-studio-wasm.sh

wasm-parity: ## Native vs WebAssembly correlator output must be identical
	$(CARGO) run --quiet --locked -p wyrd-correlator --features synth --example parity_fixtures -- target/wasm-parity
	$(CARGO) build --quiet --locked --release -p wyrd-wasm --target wasm32-unknown-unknown
	wasm-bindgen --target nodejs --out-dir target/wasm-parity/pkg target/wasm32-unknown-unknown/release/wyrd_wasm.wasm
	node scripts/wasm-parity.mjs target/wasm-parity target/wasm-parity/pkg

FUZZ_TARGETS := bundle_reader event_cbor manifest
FUZZ_SECONDS ?= 60
# The host triple: a prebuilt cargo-fuzz (CI) otherwise defaults to the musl
# target it was built for, which the sanitizers do not support.
FUZZ_TRIPLE ?= $(shell rustc -vV | sed -n 's/^host: //p')
fuzz: ## Fuzz every untrusted-input parser for FUZZ_SECONDS each (nightly + cargo-fuzz)
	@for t in $(FUZZ_TARGETS); do \
		mkdir -p fuzz/corpus/$$t && \
		echo "fuzz $$t for $(FUZZ_SECONDS) s" && \
		$(CARGO) +nightly fuzz run -O --target $(FUZZ_TRIPLE) $$t fuzz/corpus/$$t fuzz/seeds/$$t -- \
			-max_total_time=$(FUZZ_SECONDS) -rss_limit_mb=2048 -max_len=65536 -print_final_stats=1 || exit 1; \
	done

fuzz-seeds: ## Regenerate the committed fuzz seed corpora from real exports
	$(CARGO) run --quiet --locked -p wyrd-bundle --example fuzz_seeds -- fuzz/seeds

secrets: ## Secret scan of the working tree and history
	gitleaks detect --no-banner --redact

check: fmt-check lint typecheck test deny secrets ## Local gate (subset available so far)

schemas: ## Regenerate event and daemon API schemas, OpenAPI, and TypeScript types
	$(CARGO) run --quiet --locked -p wyrd-schema --bin wyrd-gen-schemas -- .
	$(CARGO) run --quiet --locked -p wyrdd --bin wyrdd-gen-api -- .

schemas-check: schemas ## Fail if generated schema files are stale
	@git add -N packages/schemas docs/schemas docs/api >/dev/null 2>&1 || true
	@git diff --exit-code --stat -- packages/schemas docs/schemas docs/api || { echo "Generated schemas are stale: run 'make schemas' and commit."; exit 1; }
