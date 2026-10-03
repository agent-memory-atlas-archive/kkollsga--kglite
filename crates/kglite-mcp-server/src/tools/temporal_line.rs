//! The one-line `temporal:` echo under a Cypher result.
//!
//! A graph that declares validity echoes on nearly every statement, so the
//! line is a summary: where the context came from, the instant, the route,
//! the total hidden count with the three heaviest targets, and the
//! endpoint-invalid count. The per-target map stays in the structured
//! diagnostics.

use kglite::api::cypher::TemporalDiagnostics;

/// How many hidden targets the line names before folding the rest.
const NAMED_TARGETS: usize = 3;

pub(super) fn temporal_line(echo: &TemporalDiagnostics) -> String {
    let mut line = format!(
        "{} as of {}, route {}",
        echo.source, echo.instant, echo.route
    );
    if echo.instant == "all" {
        line = format!("{}, every version, route {}", echo.source, echo.route);
    }
    let total: usize = echo.hidden.values().sum();
    if total > 0 {
        let mut ranked: Vec<(&String, &usize)> =
            echo.hidden.iter().filter(|(_, n)| **n > 0).collect();
        ranked.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let mut named: Vec<String> = ranked
            .iter()
            .take(NAMED_TARGETS)
            .map(|(target, n)| format!("{target} {n}"))
            .collect();
        if ranked.len() > NAMED_TARGETS {
            named.push(format!("+{} more", ranked.len() - NAMED_TARGETS));
        }
        line.push_str(&format!("; hidden {total}: {}", named.join(", ")));
    } else if !echo.hidden.is_empty() {
        line.push_str("; hidden 0");
    }
    if let Some(invalid) = echo.endpoint_invalid {
        line.push_str(&format!("; endpoint_invalid {invalid}"));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo(hidden: &[(&str, usize)]) -> TemporalDiagnostics {
        TemporalDiagnostics {
            axis: "VALID_TIME".into(),
            source: "default".into(),
            instant: "2026-10-03".into(),
            route: "guarded".into(),
            hidden: hidden.iter().map(|(t, n)| (t.to_string(), *n)).collect(),
            endpoint_invalid: Some(2),
            ..TemporalDiagnostics::default()
        }
    }

    #[test]
    fn the_heaviest_three_targets_are_named_and_the_rest_folded() {
        let line = temporal_line(&echo(&[
            ("(:A)", 1),
            ("(:B)", 9),
            ("(:C)", 5),
            ("(:D)", 7),
            ("(:E)", 0),
        ]));
        assert_eq!(
            line,
            "default as of 2026-10-03, route guarded; hidden 22: (:B) 9, (:D) 7, (:C) 5, +1 more; endpoint_invalid 2"
        );
    }

    #[test]
    fn nothing_hidden_and_the_all_form_stay_short() {
        let mut none = echo(&[("(:A)", 0)]);
        none.endpoint_invalid = None;
        assert_eq!(
            temporal_line(&none),
            "default as of 2026-10-03, route guarded; hidden 0"
        );
        let all = TemporalDiagnostics {
            source: "skipped:write".into(),
            instant: "all".into(),
            route: "plain".into(),
            ..TemporalDiagnostics::default()
        };
        assert_eq!(
            temporal_line(&all),
            "skipped:write, every version, route plain"
        );
    }
}
