SHELL := /bin/sh
SDK_VERSION := 0.0.19

.PHONY: preflight lock prepare build verify require-tooling require-out release attest

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

require-tooling:
	@case "$(origin SDK_ROOT):$(origin SDK_RELEASE)" in "command line:command line") ;; *) echo 'SDK_ROOT and SDK_RELEASE must be exact command-line inputs' >&2; exit 64 ;; esac
	@case "$(SDK_ROOT):$(SDK_RELEASE)" in /*:/*) ;; *) echo 'SDK_ROOT and SDK_RELEASE must be absolute paths' >&2; exit 64 ;; esac
	@test -d "$(SDK_ROOT)" && test ! -L "$(SDK_ROOT)" && test -x "$(SDK_ROOT)/bin/soksak-sdk" && test ! -L "$(SDK_ROOT)/bin/soksak-sdk" || { echo 'SDK_ROOT is not an extracted regular SDK release' >&2; exit 78; }
	@test -f "$(SDK_RELEASE)" && test ! -L "$(SDK_RELEASE)" || { echo 'SDK_RELEASE is not a regular file' >&2; exit 78; }
	@package_version="$$(node -e 'process.stdout.write(require(process.argv[1]).version)' "$(SDK_ROOT)/package.json")"; \
		release_version="$$(node -e 'process.stdout.write(require(process.argv[1]).version)' "$(SDK_RELEASE)")"; \
		test "$$package_version" = "$(SDK_VERSION)" && test "$$release_version" = "$(SDK_VERSION)" || { echo "TOOLCHAIN_MISMATCH soksak-sdk required=$(SDK_VERSION) package=$$package_version release=$$release_version" >&2; exit 78; }

require-out:
	@case "$(origin OUT)" in "command line") ;; *) echo 'OUT must be an absolute command-line release directory' >&2; exit 64 ;; esac
	@case "$(OUT)" in /*) ;; *) echo 'OUT must be an absolute path' >&2; exit 64 ;; esac
	@test "$(OUT)" != "$(CURDIR)" || { echo 'OUT must not replace the source repository' >&2; exit 64; }

release: require-tooling require-out verify
	@dirty="$$(git status --porcelain)"; test -z "$$dirty" || { printf '%s\n' "$$dirty" >&2; echo 'release source checkout must be clean' >&2; exit 65; }
	@"$(SDK_ROOT)/bin/soksak-sdk" package --root "$(CURDIR)" \
		--spec-root "$(SDK_ROOT)/.dependencies/soksak-spec" \
		--commit "$$(git rev-parse --verify HEAD)" --out "$(OUT)"

attest: release
	@platform="$$(node -p 'process.platform')"; architecture="$$(node -p 'process.arch')"; \
		rust_version="$$(rustc --version | awk '{print $$2}')"; python_version="$$(python3 --version | awk '{print $$2}')"; \
		"$(SDK_ROOT)/bin/soksak-sdk" attest --release-dir "$(OUT)" \
		--spec-root "$(SDK_ROOT)/.dependencies/soksak-spec" --tooling-release "$(SDK_RELEASE)" \
		--mode native --platform "$$platform" --architecture "$$architecture" \
		--tool "rust=$$rust_version" --tool "python=$$python_version"
