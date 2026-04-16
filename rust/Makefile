.PHONY: run test lint fmt fmt-check clean bundle

run:
	cargo run -- --auto-start

test:
	cargo test --workspace --exclude sdrapp-sdrplay --exclude sdrapp-sdrplay-sys

lint:
	cargo clippy --workspace --exclude sdrapp-sdrplay --exclude sdrapp-sdrplay-sys -- -D warnings

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

bundle:
	bash bundle_macos.sh

clean:
	cargo clean
	rm -rf dist/
