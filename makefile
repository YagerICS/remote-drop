test:
	cargo test --no-default-features --features=loom --release
	MIRIFLAGS=-Zmiri-ignore-leaks cargo miri test --no-default-features --features=embassy --release
	MIRIFLAGS=-Zmiri-ignore-leaks cargo miri test --no-default-features --features=std --release
