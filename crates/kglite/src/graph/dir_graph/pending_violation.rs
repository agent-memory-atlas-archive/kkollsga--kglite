//! The typed side channel for write refusals: a structured constraint or
//! ontology violation parked beside the message that the engine's
//! `Result<_, String>` error channel carries, recovered by message identity
//! when the binding builds its typed error.

use super::DirGraph;

/// A structured write refusal parked beside its message while the
/// `Result<_, String>` channel unwinds (see `pending_constraint_violation`).
#[derive(Clone)]
pub(crate) enum PendingViolation {
    Constraint(crate::graph::constraints::ConstraintViolation),
    Ontology(crate::graph::ontology::violation::OntologyViolation),
    Declaration(crate::graph::ontology::violation::OntologyDeclarationRefused),
}

impl DirGraph {
    /// Stringify `violation` for the `Result<_, String>` error channel while
    /// parking the structured value in
    /// [`Self::pending_constraint_violation`], so the adapter that ultimately
    /// builds the typed error can recover the constraint kind, node type,
    /// properties, and descriptor instead of only the prose.
    ///
    /// Returns the message to hand to `Err(..)`; call it at every site that
    /// would otherwise write `.map_err(|v| v.to_string())`.
    pub(crate) fn record_constraint_violation(
        &mut self,
        violation: crate::graph::constraints::ConstraintViolation,
    ) -> String {
        let message = violation.to_string();
        self.pending_constraint_violation = Some(Box::new((
            message.clone(),
            PendingViolation::Constraint(violation),
        )));
        message
    }

    /// [`Self::record_constraint_violation`] for an ontology refusal: parks
    /// the violation under its own message and returns that message for
    /// `Err(..)`. The write gates call this wherever they would otherwise
    /// stringify an [`OntologyViolation`](crate::graph::ontology::violation::OntologyViolation).
    pub(crate) fn record_ontology_violation(
        &mut self,
        violation: crate::graph::ontology::violation::OntologyViolation,
    ) -> String {
        let message = violation.message.clone();
        self.pending_constraint_violation = Some(Box::new((
            message.clone(),
            PendingViolation::Ontology(violation),
        )));
        message
    }

    /// [`Self::record_ontology_violation`] for a declaration refused over
    /// stored data: parks the report under its message, which the caller must
    /// hand to `Err(..)` unchanged for the identity check to recover it.
    pub(crate) fn record_declaration_refusal(
        &mut self,
        refusal: crate::graph::ontology::violation::OntologyDeclarationRefused,
    ) -> String {
        let message = refusal.message.clone();
        self.pending_constraint_violation = Some(Box::new((
            message.clone(),
            PendingViolation::Declaration(refusal),
        )));
        message
    }

    /// Clear any parked violation. Called before an execution begins so a
    /// violation left by an earlier run on the same working copy can never be
    /// attributed to a later, unrelated error.
    pub(crate) fn clear_pending_constraint_violation(&mut self) {
        self.pending_constraint_violation = None;
    }

    /// Take the parked violation **only if** it is the one behind `message`.
    ///
    /// The identity check is what makes the side channel safe: the `String` is
    /// still the control-flow channel, so if any intermediate frame wrapped or
    /// replaced the message, the parked violation no longer describes the error
    /// being reported and is dropped rather than mis-attributed. This is an
    /// equality test against a string this graph itself produced — not a
    /// pattern match on error prose.
    #[cfg(test)]
    pub(crate) fn take_constraint_violation_for(
        &mut self,
        message: &str,
    ) -> Option<crate::graph::constraints::ConstraintViolation> {
        match self.take_pending_violation_for(message)? {
            PendingViolation::Constraint(violation) => Some(violation),
            PendingViolation::Ontology(_) | PendingViolation::Declaration(_) => None,
        }
    }

    /// Take the parked violation of either kind, under the same
    /// message-identity rule as [`Self::take_constraint_violation_for`].
    pub(crate) fn take_pending_violation_for(&mut self, message: &str) -> Option<PendingViolation> {
        let parked = self.pending_constraint_violation.take()?;
        let (recorded, violation) = *parked;
        (recorded == message).then_some(violation)
    }

    /// Typed [`KgError`] for a write that failed with `message`, when that
    /// failure was a declared-constraint or ontology violation.
    ///
    /// Returns `None` when the error was something else, so each caller keeps
    /// its own fallback: the Cypher path degrades to
    /// [`KgError::CypherExecution`], the bulk-loader path to
    /// [`KgError::Argument`]. Every binding that surfaces a write error over
    /// the engine's `Result<_, String>` channel needs exactly this step, so it
    /// lives here rather than being re-derived per binding.
    pub fn take_constraint_error(&mut self, message: &str) -> Option<crate::error::KgError> {
        self.take_pending_violation_for(message)
            .map(|parked| match parked {
                PendingViolation::Constraint(v) => crate::error::KgError::from(v),
                PendingViolation::Ontology(v) => crate::error::KgError::from(v),
                PendingViolation::Declaration(r) => crate::error::KgError::from(r),
            })
    }
}
