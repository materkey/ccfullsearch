.PHONY: check fmt clippy test

check: fmt clippy test

fmt:
	cargo fmt --check

clippy:
	cargo clippy --all-targets --all-features -- -D warnings

test:
	cargo test
