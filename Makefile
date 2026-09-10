.PHONY: check test fmt lint install clean

check: fmt lint test

test:
	cargo test

fmt:
	cargo fmt --check

lint:
	cargo clippy --all-targets -- -D warnings

install:
	cargo install --path . --force

clean:
	cargo clean
