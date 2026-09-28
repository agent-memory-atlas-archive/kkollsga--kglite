//! Validity intervals: the declarations that name a type's two bound
//! properties, the evaluator every surface reads bounds through ([`eval`]),
//! the endpoint indexes and masks that answer an instant without reading
//! bounds, and the valid-time view and slice. The filter queries and fluent
//! steps run under is `core::graph_filter::ElementFilter`; a bound that holds
//! anything but a date, a datetime or an ISO string is an error naming the
//! element and the property, not a pass.

pub(crate) mod declarations;
#[cfg(test)]
mod declarations_tests;
pub(crate) mod duplicate_ids;
pub(crate) mod endpoint_index;
pub(crate) mod eval;
pub(crate) mod instant;
mod loader;
#[cfg(test)]
mod loader_tests;
mod merge_key;
#[cfg(test)]
mod merge_key_tests;
pub(crate) mod persist;
#[cfg(test)]
mod persist_tests;
mod request;
pub(crate) mod slice;
mod validate;
pub(crate) mod vector_mask;
pub mod view;

pub(crate) use declarations::declared;
pub(crate) use declarations::merge_start_key;
pub use declarations::{
    declare, declare_loaded, edge_configs, list, node_config, undeclare, DeclarationInfo,
    DeclareReport, TemporalTarget, DISK_NODE_ABUTMENT_CAP,
};
pub use eval::IntervalConvention;
pub(crate) use loader::{adopt_declarations, settle_adopted, withdraw_adopted};
pub use loader::{declare_defaulted, declare_from_column_types, LoadDeclaration};
pub(crate) use merge_key::{Image, Start, StartKey};
pub use request::{
    node_request_config, node_type_has_property, relationship_request_configs,
    relationship_type_has_property, unknown_bound_message,
};
pub(crate) use validate::{edge_bound, node_bound, view_bound};

use eval::{BoundSide, TemporalError};

/// `property 'vf': the from bound … is not a date …` for a declaration known
/// by its two bound property names — the element prefix is the caller's,
/// since only it knows which element the properties belong to.
pub(crate) fn describe_bound_error(err: TemporalError, from: &str, to: &str) -> String {
    let property = match &err {
        TemporalError::Bound {
            side: BoundSide::From,
            ..
        } => from,
        TemporalError::Bound {
            side: BoundSide::To,
            ..
        } => to,
        TemporalError::Instant { .. } => return err.to_string(),
    };
    format!("property '{property}': {err}")
}
