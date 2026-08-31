#![cfg_attr(target_arch = "wasm32", no_std)]

#[cfg(not(target_family = "wasm"))]
pub mod budget;
