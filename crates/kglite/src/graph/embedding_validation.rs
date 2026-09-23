//! Shared validation for vectors crossing an embedding boundary.

use std::fmt;

/// The first non-finite coordinate in a vector.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct NonFiniteVector {
    pub(crate) index: usize,
    pub(crate) value: f32,
}

impl fmt::Display for NonFiniteVector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "vector coordinate {} must be finite (got {})",
            self.index, self.value
        )
    }
}

/// Reject NaN and either infinity before they can enter score ordering, cached
/// norms, an ANN index, or persisted storage.
pub(crate) fn validate_finite_vector(vector: &[f32]) -> Result<(), NonFiniteVector> {
    match vector.iter().position(|value| !value.is_finite()) {
        Some(index) => Err(NonFiniteVector {
            index,
            value: vector[index],
        }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_the_first_nonfinite_coordinate() {
        let error = validate_finite_vector(&[1.0, f32::INFINITY, f32::NAN]).unwrap_err();
        assert_eq!(error.index, 1);
        assert_eq!(error.value, f32::INFINITY);
        assert_eq!(
            error.to_string(),
            "vector coordinate 1 must be finite (got inf)"
        );
    }
}
