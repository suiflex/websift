# Local convenience wrapper for the checks CI runs.
#
# .github/workflows/ci.yml is the authority. These targets duplicate it on purpose: CI splits the
# Rust chain across Linux, macOS, and Windows, and `make` is not dependably present on the Windows
# runner, so CI cannot simply call these. If you change the gate, change it in both places.

.DEFAULT_GOAL := help
.PHONY: help check fmt fmt-check lint test worker run

help: ## List targets
	@grep -hE '^[a-z-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk -F':.*?## ' '{printf "  \033[1m%-10s\033[0m %s\n", $$1, $$2}'

check: fmt-check test lint worker ## Everything CI gates on

fmt: ## Format
	cargo fmt

fmt-check: ## Check formatting
	cargo fmt --check

lint: ## Clippy, warnings are errors
	cargo clippy --all-targets -- -D warnings

test: ## Compile every target and run the test suite
	cargo check --all-targets
	cargo test

worker: ## Browser worker tests, worker schema, and the npm asset-name mapping
	npm test --prefix browser-worker
	jq empty schemas/worker-v1.schema.json
	node npm/install.js --selftest

run: ## Run the MCP server on a scratch profile
	cargo run -- mcp --profile dev
