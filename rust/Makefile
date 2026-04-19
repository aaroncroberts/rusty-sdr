.PHONY: run test lint fmt fmt-check clean bundle

run:
	cargo run -- --auto-start

test:
	cargo test --workspace --exclude rusty-sdr-sdrplay --exclude rusty-sdr-sdrplay-sys

lint:
	cargo clippy --workspace --exclude rusty-sdr-sdrplay --exclude rusty-sdr-sdrplay-sys -- -D warnings

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

bundle:
	bash bundle_macos.sh

clean:
	cargo clean
	rm -rf dist/
