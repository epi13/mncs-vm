//! Canonical machine-native virtual machine and execution runtime for MNCS.
//!
//! The VM executes compiler-produced MNCS VM artifacts while preserving
//! runtime-relevant semantics: artifact identity, call identity, explicit
//! effects, capabilities, resource bounds, structured failure,
//! execution-local memory, and evidence-bearing observations.
//!
//! [`artifact`] defines the canonical artifact contract (`mncs.vm.artifact/1`).
//! [`admit`] loads and admits artifacts with typed refusals. [`engine`]
//! is the reference execution core over admitted artifacts. [`capability`]
//! enforces authority at the effect boundary; [`resource`] enforces
//! budgets; [`outcome`] and [`evidence`] make results machine-readable.
//! [`migrate`] is the explicit temporary adapter that consumes current
//! research-bytecode payloads (read-only upstream) until the compiler
//! emits the canonical artifact directly.

pub mod admit;
pub mod artifact;
pub mod capability;
pub mod debug;
pub mod engine;
pub mod evidence;
pub mod harness;
pub mod migrate;
pub mod outcome;
pub mod resource;
pub mod session;
pub mod value;
