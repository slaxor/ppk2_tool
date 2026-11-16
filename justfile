test:
    cargo test --workspace

build:
    cargo build --workspace --release

doc:
    cargo doc --workspace --no-deps --open

fmt:
    cargo fmt --all

lint:
    cargo clippy --workspace --all-targets -- -D warnings

fix:
    cargo clippy --workspace --all-targets --fix --allow-dirty
    cargo fmt --all

run *args:
    cargo build --workspace
    target/debug/ppk2_tool-gui {{args}}

cli *args:
    cargo run -p ppk2_tool-cli -- {{args}}

serve *args:
    cargo run -p ppk2_tool-cli -- serve {{args}}
