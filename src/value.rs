//! VM-owned machine value model.
//!
//! Values are the execution boundary: requests marshal into them,
//! the engine computes over them, results marshal out of them.
//! Conversion to and from the upstream wire value is explicit here
//! so the engine core never imports compiler types.

use std::sync::Arc;

/// One runtime value. Logical, not physical: no layout, offset, or
/// host address participates in meaning.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Value {
    Integer {
        value: i128,
        bits: u32,
        signed: bool,
    },
    Boolean(bool),
    Byte(u8),
    /// IEEE-754 binary64 bits. Carried, never computed: float
    /// arithmetic is an explicit engine UNSUPPORTED.
    FloatBits(u64),
    Finite {
        type_id: String,
        variant_id: String,
        discriminant: u32,
        payload: Arc<Vec<(String, Value)>>,
    },
    Record {
        type_id: String,
        name: String,
        fields: Arc<Vec<(String, Value)>>,
    },
    Sequence {
        elements: Arc<Vec<Value>>,
    },
    Vector {
        elements: Arc<Vec<Value>>,
    },
    Mask {
        lanes: Arc<Vec<bool>>,
    },
}

impl Value {
    /// Conservative cell count for memory accounting. Containers
    /// count their elements recursively with a hard cap so a
    /// hostile shape cannot overflow the meter itself.
    pub fn cells(&self) -> u64 {
        self.cells_capped(1_000_000)
    }

    fn cells_capped(&self, cap: u64) -> u64 {
        match self {
            Value::Integer { .. } | Value::Boolean(_) | Value::Byte(_) | Value::FloatBits(_) => 1,
            Value::Finite { payload, .. } => {
                let mut total = 1u64;
                for (_, field) in payload.iter() {
                    total = total.saturating_add(field.cells_capped(cap));
                    if total >= cap {
                        return cap;
                    }
                }
                total
            }
            Value::Record { fields, .. } => {
                let mut total = 1u64;
                for (_, field) in fields.iter() {
                    total = total.saturating_add(field.cells_capped(cap));
                    if total >= cap {
                        return cap;
                    }
                }
                total
            }
            Value::Sequence { elements } | Value::Vector { elements } => {
                let mut total = 1u64;
                for element in elements.iter() {
                    total = total.saturating_add(element.cells_capped(cap));
                    if total >= cap {
                        return cap;
                    }
                }
                total
            }
            Value::Mask { lanes } => 1u64.saturating_add(lanes.len() as u64),
        }
    }

    /// Short kind tag for evidence and diagnostics.
    pub fn kind_tag(&self) -> &'static str {
        match self {
            Value::Integer { .. } => "integer",
            Value::Boolean(_) => "boolean",
            Value::Byte(_) => "byte",
            Value::FloatBits(_) => "float",
            Value::Finite { .. } => "finite",
            Value::Record { .. } => "record",
            Value::Sequence { .. } => "sequence",
            Value::Vector { .. } => "vector",
            Value::Mask { .. } => "mask",
        }
    }
}

/// Marshal an upstream wire value into the VM model.
pub fn from_wire(value: &mncs_model::ExecutionValue) -> Value {
    use mncs_model::ExecutionValue as W;
    match value {
        W::Integer { value, ty } => Value::Integer {
            value: *value,
            bits: u32::from(ty.bits),
            signed: ty.signed,
        },
        W::Boolean { value } => Value::Boolean(*value),
        W::Byte { value } => Value::Byte((*value).clamp(0, 255) as u8),
        W::Float { bits, .. } => Value::FloatBits(*bits),
        W::Finite {
            type_identity,
            variant_identity,
            discriminant,
            payload,
        } => Value::Finite {
            type_id: type_identity.0.clone(),
            variant_id: variant_identity.0.clone(),
            discriminant: *discriminant,
            payload: Arc::new(
                payload
                    .iter()
                    .map(|(name, item)| (name.clone(), from_wire(item)))
                    .collect(),
            ),
        },
        W::Record {
            type_identity,
            name,
            fields,
        } => Value::Record {
            type_id: type_identity.0.clone(),
            name: name.clone(),
            fields: Arc::new(
                fields
                    .iter()
                    .map(|(name, item)| (name.clone(), from_wire(item)))
                    .collect(),
            ),
        },
        W::Sequence { values } => Value::Sequence {
            elements: Arc::new(values.iter().map(from_wire).collect()),
        },
        W::Vector { values } => Value::Vector {
            elements: Arc::new(values.iter().map(from_wire).collect()),
        },
        W::Mask { lanes } => Value::Mask {
            lanes: lanes.clone(),
        },
    }
}

/// Marshal a VM value back to the upstream wire value for results,
/// evidence, and differential comparison.
pub fn to_wire(value: &Value) -> mncs_model::ExecutionValue {
    use mncs_model::{ExecutionValue as W, IntegerType, SemanticId};
    match value {
        Value::Integer {
            value,
            bits,
            signed,
        } => W::Integer {
            value: *value,
            ty: IntegerType {
                bits: (*bits).min(126) as u16,
                signed: *signed,
            },
        },
        Value::Boolean(value) => W::Boolean { value: *value },
        Value::Byte(value) => W::Byte {
            value: i128::from(*value),
        },
        Value::FloatBits(bits) => W::Float {
            bits: *bits,
            ty: mncs_model::FloatType::f64(),
        },
        Value::Finite {
            type_id,
            variant_id,
            discriminant,
            payload,
        } => W::Finite {
            type_identity: SemanticId(type_id.clone()),
            variant_identity: SemanticId(variant_id.clone()),
            discriminant: *discriminant,
            payload: Arc::new(
                payload
                    .iter()
                    .map(|(name, item)| (name.clone(), to_wire(item)))
                    .collect(),
            ),
        },
        Value::Record {
            type_id,
            name,
            fields,
        } => W::Record {
            type_identity: SemanticId(type_id.clone()),
            name: name.clone(),
            fields: Arc::new(
                fields
                    .iter()
                    .map(|(name, item)| (name.clone(), to_wire(item)))
                    .collect(),
            ),
        },
        Value::Sequence { elements } => W::Sequence {
            values: Arc::new(elements.iter().map(to_wire).collect()),
        },
        Value::Vector { elements } => W::Vector {
            values: Arc::new(elements.iter().map(to_wire).collect()),
        },
        Value::Mask { lanes } => W::Mask {
            lanes: lanes.clone(),
        },
    }
}
