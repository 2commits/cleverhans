//! Registry codegen across the language seam (spec §9,
//! "adoption-critical").
//!
//! The registry is defined once — programmatically in Rust or as a
//! declarative document ([`cleverhans_core::schema::RegistrySchema`]) — and
//! this crate emits matching typed modules so "add an action" is one registry
//! edit and every frontend/backend is type-safe for free:
//!
//! - [`typescript_module`] — TS string-literal unions + interfaces
//! - [`python_module`] — Python `Literal` unions + `TypedDict`s (3.11+)
//!
//! Apps run the bundled CLI (`cargo run -p cleverhans-codegen -- --schema
//! registry.json --ts out.ts --py out.py`) or call the emitters from a build
//! script.

mod py;
mod ts;

pub use py::python_module;
pub use ts::typescript_module;

/// Converts an inert key like `transaction.coBuyer.remove` into an
/// identifier prefix like `TransactionCoBuyerRemove`.
///
/// Splits on any non-alphanumeric character and on lowercase→uppercase
/// boundaries, then PascalCases the segments — the ID itself stays an opaque
/// string everywhere; this name is presentation only.
#[must_use]
pub fn pascal_ident(id: &str) -> String {
    id.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|segment| !segment.is_empty())
        .flat_map(split_camel_boundaries)
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_ascii_uppercase().to_string() + &chars.as_str().to_ascii_lowercase()
            })
        })
        .collect()
}

fn split_camel_boundaries(segment: &str) -> Vec<&str> {
    let mut words = Vec::new();
    let mut start = 0;
    for (index, c) in segment.char_indices().skip(1) {
        let prev_is_lower = segment[..index]
            .chars()
            .next_back()
            .is_some_and(|p| p.is_ascii_lowercase());
        if c.is_ascii_uppercase() && prev_is_lower {
            words.push(&segment[start..index]);
            start = index;
        }
    }
    words.push(&segment[start..]);
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    mod pascal_ident {
        use super::*;

        #[test]
        fn splits_dots_and_camel_case() {
            assert_eq!(
                pascal_ident("transaction.coBuyer.remove"),
                "TransactionCoBuyerRemove"
            );
        }

        #[test]
        fn handles_snake_and_kebab() {
            assert_eq!(
                pascal_ident("bulk_ops.delete-by-predicate"),
                "BulkOpsDeleteByPredicate"
            );
        }
    }
}
