MIRI_NIGHTLY=nightly-$(curl -s https://rust-lang.github.io/rustup-components-history/x86_64-unknown-linux-gnu/miri)
echo "Installing latest nightly with Miri: $MIRI_NIGHTLY"
rustup set profile minimal
rustup default "$MIRI_NIGHTLY"
rustup component add miri
yes | cargo miri test --features="std"