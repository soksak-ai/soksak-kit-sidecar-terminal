SHELL := /bin/sh

.PHONY: preflight lock prepare build verify

preflight:
	@scripts/check-build-environment.sh

lock: preflight
	@cargo metadata --format-version 1 > /dev/null

prepare: preflight
	@cargo fetch --locked

build: prepare
	@cargo build --locked --release

verify: prepare
	@cargo test --locked
	@python3 scripts/test_install_pty_release.py
