#![no_std]

#[cfg(not(target_family = "wasm"))]
pub mod budget;
