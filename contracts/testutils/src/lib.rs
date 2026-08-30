#![cfg_attr(target_arch = "wasm32", no_std)]

#[cfg(not(target_arch = "wasm32"))]
pub mod budget;

#[cfg(target_arch = "wasm32")]
pub mod budget;
