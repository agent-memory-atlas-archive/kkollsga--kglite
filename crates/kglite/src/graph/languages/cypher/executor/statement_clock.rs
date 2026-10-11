//! The instant the no-argument temporal constructors (`datetime()`, `date()`,
//! `localtime()`, …) and the default valid-time context report: one reading per
//! statement, shared by every executor the statement spawns through the
//! [`ExecutionBudget`](super::budget::ExecutionBudget) they all hold. Neo4j
//! holds the clock constant for a statement, so `datetime() = datetime()` is
//! true and a row-by-row `SET n.t = datetime()` stamps one instant.

use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone, Utc};

/// A clock reading, in UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StatementClock {
    utc: NaiveDateTime,
}

impl StatementClock {
    /// Read the system clock.
    pub(crate) fn read() -> Self {
        Self {
            utc: Utc::now().naive_utc(),
        }
    }

    /// A fixed reading, for tests that need a known "now".
    #[cfg(test)]
    pub(crate) fn at(utc: NaiveDateTime) -> Self {
        Self { utc }
    }

    /// The reading as a UTC wall clock.
    pub(crate) fn utc(self) -> NaiveDateTime {
        self.utc
    }

    /// The same instant on the machine's local wall clock.
    pub(crate) fn local(self) -> NaiveDateTime {
        Local.from_utc_datetime(&self.utc).naive_local()
    }

    /// Today's date in UTC.
    pub(crate) fn today(self) -> NaiveDate {
        self.utc.date()
    }
}
