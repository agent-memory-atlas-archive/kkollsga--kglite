use crate::graph::diagnostics::Diagnostic;
use std::collections::VecDeque;

const MAX_REPORT_HISTORY: usize = 10;

// Each variant wraps the like-named `*OperationReport` struct; the shared
// `Operation` suffix mirrors those payload types.
#[derive(Debug, Clone)]
#[allow(clippy::enum_variant_names)]
pub enum OperationReport {
    NodeOperation(NodeOperationReport),
    ConnectionOperation(ConnectionOperationReport),
    CalculationOperation(CalculationOperationReport),
}

#[derive(Debug, Clone)]
pub struct CalculationOperationReport {
    pub operation_type: String,
    pub expression: String,
    pub nodes_processed: usize,
    pub nodes_updated: usize,
    pub nodes_with_errors: usize,
    pub processing_time_ms: f64,
    pub is_aggregation: bool,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub errors: Vec<String>,
    /// Advisories about the rows the stored result wrote, such as a validity
    /// interval left empty. Not errors: the rows are stored.
    pub warnings: Vec<String>,
    /// `warnings` with each entry's classification, in the same order.
    pub diagnostics: Vec<Diagnostic>,
}

impl CalculationOperationReport {
    pub fn new(
        operation_type: String,
        expression: String,
        nodes_processed: usize,
        nodes_updated: usize,
        nodes_with_errors: usize,
        processing_time_ms: f64,
        is_aggregation: bool,
    ) -> Self {
        Self {
            operation_type,
            expression,
            nodes_processed,
            nodes_updated,
            nodes_with_errors,
            processing_time_ms,
            is_aggregation,
            timestamp: chrono::Utc::now(),
            errors: Vec::new(),
            warnings: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    /// Record each advisory in both `warnings` and `diagnostics`.
    pub fn warn_all(&mut self, diagnostics: impl IntoIterator<Item = Diagnostic>) {
        for diagnostic in diagnostics {
            self.warnings.push(diagnostic.message.clone());
            self.diagnostics.push(diagnostic);
        }
    }

    pub fn with_errors(mut self, errors: Vec<String>) -> Self {
        self.errors = errors;
        self
    }
}

#[derive(Debug, Clone)]
pub struct NodeOperationReport {
    pub operation_type: String,
    pub nodes_created: usize,
    pub nodes_updated: usize,
    pub nodes_skipped: usize,
    pub processing_time_ms: f64,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub errors: Vec<String>,
    /// Advisories about rows the call wrote, such as rows whose validity
    /// interval is empty under a `half_open` declaration. Not errors: the
    /// rows are stored.
    pub warnings: Vec<String>,
    /// `warnings` with each entry's classification, in the same order.
    pub diagnostics: Vec<Diagnostic>,
}

impl NodeOperationReport {
    pub fn new(
        operation_type: String,
        nodes_created: usize,
        nodes_updated: usize,
        nodes_skipped: usize,
        processing_time_ms: f64,
    ) -> Self {
        Self {
            operation_type,
            nodes_created,
            nodes_updated,
            nodes_skipped,
            processing_time_ms,
            timestamp: chrono::Utc::now(),
            errors: Vec::new(),
            warnings: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    /// Record an advisory in both `warnings` and `diagnostics`.
    pub fn warn(&mut self, diagnostic: Diagnostic) {
        self.warnings.push(diagnostic.message.clone());
        self.diagnostics.push(diagnostic);
    }

    /// [`warn`](Self::warn) each of `diagnostics`.
    pub fn warn_all(&mut self, diagnostics: impl IntoIterator<Item = Diagnostic>) {
        for diagnostic in diagnostics {
            self.warn(diagnostic);
        }
    }

    pub fn with_errors(mut self, errors: Vec<String>) -> Self {
        self.errors = errors;
        self
    }
}

#[derive(Debug, Clone)]
pub struct ConnectionOperationReport {
    pub operation_type: String,
    /// Relationships this call added.
    pub connections_created: usize,
    /// Rows that met an existing relationship of the same type between the
    /// same endpoints and merged into it per `conflict_handling` (`replace`
    /// included): the relationship already existed, so none was created.
    pub connections_updated: usize,
    pub connections_skipped: usize,
    /// Stub nodes auto-created so an edge to a missing endpoint could
    /// still connect (see `add_connections` vivification).
    pub stubs_vivified: usize,
    pub property_fields_tracked: usize,
    pub processing_time_ms: f64,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub errors: Vec<String>,
    /// Advisories about rows the call wrote, such as rows whose validity
    /// interval is empty under a `half_open` declaration. Not errors: the
    /// rows are stored.
    pub warnings: Vec<String>,
    /// `warnings` with each entry's classification, in the same order.
    pub diagnostics: Vec<Diagnostic>,
}

impl ConnectionOperationReport {
    pub fn new(
        operation_type: String,
        connections_created: usize,
        connections_skipped: usize,
        property_fields_tracked: usize,
        processing_time_ms: f64,
    ) -> Self {
        Self {
            operation_type,
            connections_created,
            connections_updated: 0,
            connections_skipped,
            stubs_vivified: 0,
            property_fields_tracked,
            processing_time_ms,
            timestamp: chrono::Utc::now(),
            errors: Vec::new(),
            warnings: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    /// Record an advisory in both `warnings` and `diagnostics`.
    pub fn warn(&mut self, diagnostic: Diagnostic) {
        self.warnings.push(diagnostic.message.clone());
        self.diagnostics.push(diagnostic);
    }

    /// [`warn`](Self::warn) each of `diagnostics`.
    pub fn warn_all(&mut self, diagnostics: impl IntoIterator<Item = Diagnostic>) {
        for diagnostic in diagnostics {
            self.warn(diagnostic);
        }
    }

    pub fn with_errors(mut self, errors: Vec<String>) -> Self {
        self.errors = errors;
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct OperationReports {
    reports: VecDeque<OperationReport>,
    last_operation_index: usize,
}

impl OperationReports {
    pub fn new() -> Self {
        OperationReports {
            reports: VecDeque::with_capacity(MAX_REPORT_HISTORY),
            last_operation_index: 0,
        }
    }

    pub fn add_report(&mut self, report: OperationReport) -> usize {
        self.last_operation_index += 1;

        self.reports.push_back(report);

        if self.reports.len() > MAX_REPORT_HISTORY {
            self.reports.pop_front();
        }

        self.last_operation_index
    }

    pub fn get_last_report(&self) -> Option<&OperationReport> {
        self.reports.back()
    }

    pub fn get_all_reports(&self) -> &VecDeque<OperationReport> {
        &self.reports
    }

    pub fn get_last_operation_index(&self) -> usize {
        self.last_operation_index
    }
}
