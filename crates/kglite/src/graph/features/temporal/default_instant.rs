//! The graph's valid-time default: the instant a statement or fluent step
//! reads when it names none. `DirGraph::valid_time_default` is the value in
//! force; `DirGraph::stored_valid_time_default` is the part saved in the
//! `.kgl` file (absent = `Today`), which a load copies into the value in force.
//! A session setter or a server flag overwrites only the value in force, so
//! the precedence is: explicit prefix or `valid_at=`, then session setter,
//! then server/manifest flag (whichever ran last), then the stored default,
//! then `Today`.

use std::fmt;

use chrono::NaiveDate;

/// Which instant an unprefixed statement (and a fluent cursor without a
/// `date()` call) reads on a graph that declares validity intervals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ValidTimeDefault {
    /// Today's UTC date, resolved per execution (the default).
    #[default]
    Today,
    /// Every version: no valid-time filtering.
    All,
    /// A fixed day.
    Date(NaiveDate),
}

impl ValidTimeDefault {
    /// Parse `today`, `all` (case-insensitive) or an ISO `YYYY-MM-DD` date.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.eq_ignore_ascii_case("today") {
            Ok(Self::Today)
        } else if text.eq_ignore_ascii_case("all") {
            Ok(Self::All)
        } else {
            NaiveDate::parse_from_str(text, "%Y-%m-%d")
                .map(Self::Date)
                .map_err(|_| {
                    format!(
                        "valid-time default '{text}' is not 'today', 'all' or a YYYY-MM-DD date"
                    )
                })
        }
    }

    /// The setting as plan-cache key material: two settings that lower a
    /// statement differently never share a code.
    pub(crate) fn cache_code(self) -> i64 {
        match self {
            Self::Today => i64::MIN,
            Self::All => i64::MIN + 1,
            Self::Date(day) => i64::from(chrono::Datelike::num_days_from_ce(&day)),
        }
    }
}

impl fmt::Display for ValidTimeDefault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Today => f.write_str("today"),
            Self::All => f.write_str("all"),
            Self::Date(day) => write!(f, "{}", day.format("%Y-%m-%d")),
        }
    }
}

impl crate::graph::schema::DirGraph {
    /// Set the default in force. With `persist`, also record it as the graph's
    /// stored default, which the next save writes into the file and a later
    /// load reads back; without it the setting lasts for this graph object.
    pub fn set_valid_time_default(&mut self, default: ValidTimeDefault, persist: bool) {
        self.valid_time_default = default;
        if persist {
            self.stored_valid_time_default = default;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_three_spellings_and_refuses_the_rest() {
        assert_eq!(
            ValidTimeDefault::parse("Today"),
            Ok(ValidTimeDefault::Today)
        );
        assert_eq!(ValidTimeDefault::parse(" ALL "), Ok(ValidTimeDefault::All));
        let day = NaiveDate::from_ymd_opt(2015, 6, 15).unwrap();
        assert_eq!(
            ValidTimeDefault::parse("2015-06-15"),
            Ok(ValidTimeDefault::Date(day))
        );
        assert!(ValidTimeDefault::parse("yesterday").is_err());
        assert!(ValidTimeDefault::parse("2015-13-01").is_err());
    }

    #[test]
    fn cache_codes_separate_every_setting() {
        let a = ValidTimeDefault::Date(NaiveDate::from_ymd_opt(2015, 6, 15).unwrap());
        let b = ValidTimeDefault::Date(NaiveDate::from_ymd_opt(2015, 6, 16).unwrap());
        let codes = [
            ValidTimeDefault::Today.cache_code(),
            ValidTimeDefault::All.cache_code(),
            a.cache_code(),
            b.cache_code(),
        ];
        for (i, x) in codes.iter().enumerate() {
            for y in &codes[i + 1..] {
                assert_ne!(x, y);
            }
        }
    }
}
