#![cfg_attr(not(any(feature = "std", test)), no_std)]
#![feature(allocator_api)]
#![feature(let_chains)]

#[cfg(not(any(feature = "std", feature = "embassy", feature = "loom")))]
compile_error!("Must specify one of the 'std' or 'embassy' or 'loom' features");

#[cfg(all(feature = "std", feature = "embassy"))]
compile_error!("Can only specify one of the 'std' or 'embassy' features");
#[cfg(all(feature = "std", feature = "loom"))]
compile_error!("Can only specify one of the 'std' or 'loom' features");
#[cfg(all(feature = "loom", feature = "embassy"))]
compile_error!("Can only specify one of the 'loom' or 'embassy' features");

pub mod remote_drop;

pub mod mutex;
