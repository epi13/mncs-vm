//! Resource envelopes and deterministic accounting.
//!
//! A `ResourceEnvelope` binds one execution: every dimension has an
//! explicit limit, every consumption step is counted by the engine,
//! and exhaustion yields a structured `BudgetExhausted` outcome naming
//! the dimension. A step counter is instruction accounting, never a
//! claim about host CPU cost.

use serde::{Deserialize, Serialize};

use crate::outcome::Outcome;

/// One bounded resource dimension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLimit {
    pub dimension: String,
    pub limit: u64,
}

/// The admitted envelope for one execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceEnvelope {
    pub limits: Vec<ResourceLimit>,
}

impl ResourceEnvelope {
    pub fn empty() -> Self {
        Self { limits: Vec::new() }
    }

    pub fn limit_of(&self, dimension: &str) -> Option<u64> {
        self.limits
            .iter()
            .find(|entry| entry.dimension == dimension)
            .map(|entry| entry.limit)
    }

    /// Merge artifact-declared bounds with caller-supplied overrides.
    /// The tighter limit wins per dimension; unknown dimensions are
    /// kept so admission (which already refused unenforceable ones)
    /// and execution agree on the envelope.
    pub fn merged(declared: &[ResourceLimit], overrides: &[ResourceLimit]) -> Self {
        let mut limits: Vec<ResourceLimit> = declared.to_vec();
        for extra in overrides {
            match limits
                .iter_mut()
                .find(|entry| entry.dimension == extra.dimension)
            {
                Some(entry) => {
                    entry.limit = entry.limit.min(extra.limit);
                }
                None => limits.push(extra.clone()),
            }
        }
        limits.sort_by(|a, b| a.dimension.cmp(&b.dimension));
        Self { limits }
    }
}

/// Live counters for one execution.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceUsage {
    pub steps: u64,
    pub call_depth: u64,
    pub max_call_depth: u64,
    pub memory_cells: u64,
    pub max_memory_cells: u64,
    pub effects: u64,
}

impl ResourceUsage {
    /// Charge one instruction step. Fails closed at the envelope.
    pub fn charge_step(&mut self, envelope: &ResourceEnvelope) -> Result<(), Outcome> {
        self.steps = self.steps.saturating_add(1);
        if let Some(limit) = envelope.limit_of("steps") {
            if self.steps > limit {
                return Err(Outcome::BudgetExhausted {
                    dimension: "steps".to_owned(),
                });
            }
        }
        Ok(())
    }

    /// Enter a call frame. Fails closed past the depth bound.
    pub fn enter_call(&mut self, envelope: &ResourceEnvelope) -> Result<(), Outcome> {
        self.call_depth = self.call_depth.saturating_add(1);
        self.max_call_depth = self.max_call_depth.max(self.call_depth);
        if let Some(limit) = envelope.limit_of("call_depth") {
            if self.call_depth > limit {
                return Err(Outcome::BudgetExhausted {
                    dimension: "call_depth".to_owned(),
                });
            }
        }
        Ok(())
    }

    pub fn exit_call(&mut self) {
        self.call_depth = self.call_depth.saturating_sub(1);
    }

    /// Charge live value cells. The engine releases dropped and
    /// overwritten values, so the count tracks live held cells,
    /// not allocation history; the peak is kept separately.
    pub fn note_cells(&mut self, cells: u64, envelope: &ResourceEnvelope) -> Result<(), Outcome> {
        self.memory_cells = self.memory_cells.saturating_add(cells);
        self.max_memory_cells = self.max_memory_cells.max(self.memory_cells);
        if let Some(limit) = envelope.limit_of("memory_cells") {
            if self.memory_cells > limit {
                return Err(Outcome::BudgetExhausted {
                    dimension: "memory_cells".to_owned(),
                });
            }
        }
        Ok(())
    }

    pub fn release_cells(&mut self, cells: u64) {
        self.memory_cells = self.memory_cells.saturating_sub(cells);
    }

    /// Charge one mediated effect.
    pub fn charge_effect(&mut self, envelope: &ResourceEnvelope) -> Result<(), Outcome> {
        self.effects = self.effects.saturating_add(1);
        if let Some(limit) = envelope.limit_of("effects") {
            if self.effects > limit {
                return Err(Outcome::BudgetExhausted {
                    dimension: "effects".to_owned(),
                });
            }
        }
        Ok(())
    }
}
