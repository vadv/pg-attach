PG_VERSION ?= 17
PG_CONFIG ?= pg_config
CARGO ?= cargo

PG_CONFIG_PATH = $(shell command -v "$(PG_CONFIG)" 2>/dev/null)
DETECTED_PG_VERSION = $(shell $(PG_CONFIG_PATH) --version 2>/dev/null | sed -E 's/^PostgreSQL ([0-9]+).*/\1/')
export PGRX_PG_CONFIG_PATH = $(PG_CONFIG_PATH)

.PHONY: all build check-pg-config test lint bdd clean

all: build

check-pg-config:
	@test -n "$(PG_CONFIG_PATH)" || { echo "pg_config not found in PATH; set PG_CONFIG" >&2; exit 1; }
	@case "$(PG_VERSION)" in 16|17|18) ;; *) echo "PG_VERSION must be 16, 17 or 18" >&2; exit 1 ;; esac
	@test "$(DETECTED_PG_VERSION)" = "$(PG_VERSION)" || { \
		echo "pg_config reports PostgreSQL $(DETECTED_PG_VERSION), requested $(PG_VERSION); fix PATH or PG_CONFIG" >&2; \
		exit 1; \
	}

build: check-pg-config
	$(CARGO) build --release --locked --no-default-features --features pg$(PG_VERSION)

test: check-pg-config
	$(CARGO) test --locked --no-default-features --features pg$(PG_VERSION)

lint: check-pg-config
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --all-targets --locked --no-default-features --features pg$(PG_VERSION) -- -D warnings -D clippy::pedantic
	$(CARGO) clippy --manifest-path tests/bdd/Cargo.toml --all-targets --locked -- -D warnings -D clippy::pedantic

bdd:
	PG_VERSION=$(PG_VERSION) ./tests/bdd/run.sh

clean:
	$(CARGO) clean
	$(CARGO) clean --manifest-path tests/bdd/Cargo.toml
