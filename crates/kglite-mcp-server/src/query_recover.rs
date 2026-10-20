//! Repair of query-route requests whose numbers overflow `f64`.
//!
//! The framework's stdio transport offers every line that fails to decode to
//! a recover hook (`StdioTransport::with_recover`). `recover_query_message`
//! is that hook: for a `tools/call` aimed at a query route it blanks any
//! float literal that is not finite inside the route's query-parameter
//! subtree, so the message decodes and the route handler can reject the
//! value precisely from the retained raw text.

use std::collections::HashMap;

use rmcp::service::RxJsonRpcMessage;
use rmcp::RoleServer;

const MAX_ACCEPTED_CONTAINER_DEPTH: usize = 127;

pub(crate) fn recover_query_message(
    raw: &str,
    query_routes: &HashMap<String, &'static [&'static str]>,
) -> Option<RxJsonRpcMessage<RoleServer>> {
    let routing = repair_nonfinite_numbers(raw, &[])?;
    let envelope: serde_json::Value = serde_json::from_slice(&routing).ok()?;
    if envelope.get("method")?.as_str()? != "tools/call" {
        return None;
    }
    let name = envelope.pointer("/params/name")?.as_str()?;
    let pointer = query_routes.get(name)?;
    let repaired = repair_nonfinite_numbers(raw, pointer)?;
    serde_json::from_slice(&repaired).ok()
}

fn repair_nonfinite_numbers(source: &str, pointer: &[&str]) -> Option<Vec<u8>> {
    let mut scanner = RepairScanner {
        source: source.as_bytes(),
        repaired: source.as_bytes().to_vec(),
        cursor: 0,
        path: Vec::new(),
        pointer,
    };
    scanner.value(0).ok()?;
    scanner.whitespace();
    (scanner.cursor == scanner.source.len()).then_some(scanner.repaired)
}

enum RepairPathPart {
    Key(String),
    Index,
}

struct RepairScanner<'a> {
    source: &'a [u8],
    repaired: Vec<u8>,
    cursor: usize,
    path: Vec<RepairPathPart>,
    pointer: &'a [&'a str],
}

impl RepairScanner<'_> {
    fn value(&mut self, depth: usize) -> Result<(), ()> {
        self.whitespace();
        match self.peek().ok_or(())? {
            b'{' if depth < MAX_ACCEPTED_CONTAINER_DEPTH => self.object(depth + 1),
            b'[' if depth < MAX_ACCEPTED_CONTAINER_DEPTH => self.array(depth + 1),
            b'"' => self.string().map(|_| ()),
            b't' => self.keyword(b"true"),
            b'f' => self.keyword(b"false"),
            b'n' => self.keyword(b"null"),
            b'-' | b'0'..=b'9' => self.number(),
            _ => Err(()),
        }
    }

    fn object(&mut self, depth: usize) -> Result<(), ()> {
        self.expect(b'{')?;
        self.whitespace();
        if self.take(b'}') {
            return Ok(());
        }
        loop {
            self.whitespace();
            let key_source = self.string()?;
            let key = serde_json::from_slice(key_source).map_err(|_| ())?;
            self.whitespace();
            self.expect(b':')?;
            self.path.push(RepairPathPart::Key(key));
            self.value(depth)?;
            self.path.pop();
            self.whitespace();
            if self.take(b'}') {
                return Ok(());
            }
            self.expect(b',')?;
        }
    }

    fn array(&mut self, depth: usize) -> Result<(), ()> {
        self.expect(b'[')?;
        self.whitespace();
        if self.take(b']') {
            return Ok(());
        }
        loop {
            self.path.push(RepairPathPart::Index);
            self.value(depth)?;
            self.path.pop();
            self.whitespace();
            if self.take(b']') {
                return Ok(());
            }
            self.expect(b',')?;
        }
    }

    fn string(&mut self) -> Result<&[u8], ()> {
        let start = self.cursor;
        self.expect(b'"')?;
        while let Some(byte) = self.peek() {
            match byte {
                b'"' => {
                    self.cursor += 1;
                    return Ok(&self.source[start..self.cursor]);
                }
                b'\\' => {
                    self.cursor += 1;
                    let escaped = self.peek().ok_or(())?;
                    self.cursor += 1;
                    if escaped == b'u' {
                        for _ in 0..4 {
                            if !self.peek().is_some_and(|byte| byte.is_ascii_hexdigit()) {
                                return Err(());
                            }
                            self.cursor += 1;
                        }
                    } else if !matches!(
                        escaped,
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't'
                    ) {
                        return Err(());
                    }
                }
                0x00..=0x1f => return Err(()),
                _ => self.cursor += 1,
            }
        }
        Err(())
    }

    fn number(&mut self) -> Result<(), ()> {
        let start = self.cursor;
        self.take(b'-');
        match self.peek().ok_or(())? {
            b'0' => self.cursor += 1,
            b'1'..=b'9' => {
                self.cursor += 1;
                while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                    self.cursor += 1;
                }
            }
            _ => return Err(()),
        }
        let mut explicit_float = false;
        if self.take(b'.') {
            explicit_float = true;
            self.digits()?;
        }
        if self.peek().is_some_and(|byte| matches!(byte, b'e' | b'E')) {
            explicit_float = true;
            self.cursor += 1;
            if self.peek().is_some_and(|byte| matches!(byte, b'+' | b'-')) {
                self.cursor += 1;
            }
            self.digits()?;
        }
        if explicit_float && self.in_target() {
            let lexeme = std::str::from_utf8(&self.source[start..self.cursor]).map_err(|_| ())?;
            if !lexeme.parse::<f64>().is_ok_and(f64::is_finite) {
                self.repaired[start] = b'0';
                self.repaired[start + 1..self.cursor].fill(b' ');
            }
        }
        Ok(())
    }

    fn in_target(&self) -> bool {
        self.path.len() >= self.pointer.len()
            && self
                .pointer
                .iter()
                .zip(&self.path)
                .all(|(wanted, actual)| match actual {
                    RepairPathPart::Key(key) => key == wanted,
                    RepairPathPart::Index => *wanted == "[]",
                })
    }

    fn keyword(&mut self, keyword: &[u8]) -> Result<(), ()> {
        if self.source.get(self.cursor..self.cursor + keyword.len()) == Some(keyword) {
            self.cursor += keyword.len();
            Ok(())
        } else {
            Err(())
        }
    }

    fn digits(&mut self) -> Result<(), ()> {
        let start = self.cursor;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.cursor += 1;
        }
        (self.cursor != start).then_some(()).ok_or(())
    }

    fn whitespace(&mut self) {
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.cursor += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.source.get(self.cursor).copied()
    }

    fn take(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, expected: u8) -> Result<(), ()> {
        self.take(expected).then_some(()).ok_or(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUERY: &[&str] = &["params", "arguments", "parameters"];

    fn routes() -> HashMap<String, &'static [&'static str]> {
        HashMap::from([("cypher_query".to_string(), QUERY)])
    }

    fn call(name: &str, arguments: &str) -> String {
        format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"{name}","arguments":{arguments}}}}}"#
        )
    }

    fn repaired(source: &str, pointer: &[&str]) -> Option<String> {
        repair_nonfinite_numbers(source, pointer).map(|b| String::from_utf8(b).unwrap())
    }

    #[test]
    fn overflowing_float_in_target_subtree_becomes_zero() {
        let raw = r#"{"a":{"b":[1e400, 2.5]}}"#;
        let out = repaired(raw, &["a", "b", "[]"]).expect("scans");
        assert_eq!(out.len(), raw.len(), "repair is length-preserving");
        let value: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["a"]["b"], serde_json::json!([0, 2.5]));
    }

    #[test]
    fn overflow_outside_target_subtree_is_left_alone() {
        let raw = r#"{"a":1e400,"b":{"c":1e400}}"#;
        let out = repaired(raw, &["b", "c"]).expect("scans");
        assert!(out.starts_with(r#"{"a":1e400,"b":{"c":0"#), "{out}");
    }

    #[test]
    fn finite_and_integer_literals_are_untouched() {
        let raw = r#"{"x":[1, 2.5, -3e2, 123456789012345678901234567890]}"#;
        assert_eq!(repaired(raw, &["x", "[]"]).as_deref(), Some(raw));
    }

    #[test]
    fn malformed_json_is_not_repaired() {
        for raw in [
            r#"{"a":1e400"#,
            r#"{"a":1e400,}"#,
            r#"{"a":01.5}"#,
            "nope",
            "",
        ] {
            assert!(repaired(raw, &[]).is_none(), "{raw}");
        }
    }

    #[test]
    fn nesting_beyond_the_accepted_depth_is_not_repaired() {
        let deep = |n: usize| format!("{}1{}", "[".repeat(n), "]".repeat(n));
        assert!(repaired(&deep(MAX_ACCEPTED_CONTAINER_DEPTH), &[]).is_some());
        assert!(repaired(&deep(MAX_ACCEPTED_CONTAINER_DEPTH + 1), &[]).is_none());
    }

    #[test]
    fn recovers_overflowing_query_parameter_on_a_query_route() {
        let raw = call(
            "cypher_query",
            r#"{"query":"RETURN $x","parameters":{"x":1e400}}"#,
        );
        let message = recover_query_message(&raw, &routes()).expect("recovered");
        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(json["params"]["arguments"]["parameters"]["x"], 0);
    }

    #[test]
    fn does_not_recover_other_tools_or_methods() {
        let other = call("graph_overview", r#"{"parameters":{"x":1e400}}"#);
        assert!(recover_query_message(&other, &routes()).is_none());
        let notification = r#"{"jsonrpc":"2.0","method":"notifications/x","params":{"v":1e400}}"#;
        assert!(recover_query_message(notification, &routes()).is_none());
    }

    #[test]
    fn does_not_recover_when_the_envelope_itself_is_unrepairable() {
        let raw = call("cypher_query", r#"{"parameters":{"x":1e400}"#);
        assert!(recover_query_message(&raw, &routes()).is_none());
    }
}
