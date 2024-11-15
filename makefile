test:
	cargo test --no-default-features --features=loom
	MIRIFLAGS=-Zmiri-ignore-leaks cargo miri test --no-default-features --features=embassy
	MIRIFLAGS=-Zmiri-ignore-leaks cargo miri test --no-default-features --features=std
