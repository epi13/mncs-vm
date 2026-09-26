//! Structured execution outcomes.
//!
//! Every terminal state of a VM run is a machine-readable variant.
//! The surrounding tooling inspects these programmatically; nothing
//! collapses into a process exit code or a free-form log line.

use serde::{Deserialize, Serialize};

/// How a VM execution terminated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    /// Returned normally with values.
    Completed,
    /// The program took a declared failure path (`Failure` terminator,
    /// failed runtime check, trapping arithmetic with a failure mode).
    ProgramFailure { mode: String, detail: String },
    /// The engine detected an internal invariant violation. Never a
    /// program state; always a VM defect signal.
    Trap { detail: String },
    /// An effect required authority the admitted environment lacks.
    CapabilityDenied {
        capability: String,
        operation: String,
    },
    /// Admitted, well-formed, but outside the v1 executable subset.
    Unsupported { feature: String },
    /// The request itself was invalid (unknown callable, bad arguments).
    InvalidRequest { reason: String },
    /// A declared resource envelope was exhausted.
    BudgetExhausted { dimension: String },
    /// An admitted provider failed the invocation.
    ProviderFailure { provider: String, detail: String },
    /// The host runtime failed (allocation, IO on evidence write, etc.).
    HostFailure { detail: String },
}

impl Outcome {
    /// Machine tag for evidence and conformance assertions.
    pub fn tag(&self) -> &'static str {
        match self {
            Outcome::Completed => "completed",
            Outcome::ProgramFailure { .. } => "program_failure",
            Outcome::Trap { .. } => "trap",
            Outcome::CapabilityDenied { .. } => "capability_denied",
            Outcome::Unsupported { .. } => "unsupported",
            Outcome::InvalidRequest { .. } => "invalid_request",
            Outcome::BudgetExhausted { .. } => "budget_exhausted",
            Outcome::ProviderFailure { .. } => "provider_failure",
            Outcome::HostFailure { .. } => "host_failure",
        }
    }

    pub fn is_completed(&self) -> bool {
        matches!(self, Outcome::Completed)
    }
}
