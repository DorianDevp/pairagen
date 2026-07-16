//! A typed wrapper attaching a [`ViolationClass`] to a contract error at its
//! construction site, so the retry loop can report which model mistake cost
//! the attempt without parsing error strings.

use loopbiotic_protocol::ViolationClass;

#[derive(Clone, Debug)]
pub struct ContractViolation {
    pub class: ViolationClass,
    pub message: String,
}

impl std::fmt::Display for ContractViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ContractViolation {}

/// Builds a classified contract error. Interchangeable with `anyhow!` at the
/// construction site; the class travels inside the error and is recovered
/// with [`violation_class`].
pub fn violation(class: ViolationClass, message: impl Into<String>) -> anyhow::Error {
    ContractViolation {
        class,
        message: message.into(),
    }
    .into()
}

/// Recovers the class from an error built with [`violation`], looking through
/// any `context(...)` layers added while the error propagated.
pub fn violation_class(error: &anyhow::Error) -> Option<ViolationClass> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ContractViolation>())
        .map(|violation| violation.class)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_survives_added_context() {
        let error = violation(ViolationClass::ContextMismatch, "context was not found")
            .context("src/work.ts");

        assert_eq!(format!("{error:#}"), "src/work.ts: context was not found");
        assert_eq!(
            violation_class(&error),
            Some(ViolationClass::ContextMismatch)
        );
    }

    #[test]
    fn unclassified_errors_have_no_class() {
        let error = anyhow::anyhow!("some plain failure");

        assert_eq!(violation_class(&error), None);
    }
}
