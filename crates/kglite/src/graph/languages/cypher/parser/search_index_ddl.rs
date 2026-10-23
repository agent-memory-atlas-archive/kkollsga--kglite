//! Cypher parser: `CREATE FULLTEXT INDEX` and `CREATE VECTOR INDEX`.
//!
//! Both statements are read structurally, in full, so a ported Neo4j schema
//! script gets the executor's named limitation (multi-label, multi-property,
//! relationship scope) instead of a syntax error. The parser only errors on
//! input it cannot read: a malformed pattern, an unknown `OPTIONS` key, a
//! property reference that does not use the pattern's variable.

use super::super::ast::*;
use super::super::tokenizer::CypherToken;
use super::CypherParser;

impl CypherParser {
    /// `FOR (n:A|B) ON EACH [n.p, …] [OPTIONS {…}]`, after the name and
    /// `IF NOT EXISTS` slots of `CREATE FULLTEXT INDEX`.
    pub(super) fn parse_fulltext_index_body(
        &mut self,
        name: Option<String>,
        if_not_exists: bool,
    ) -> Result<SchemaCommand, String> {
        let target = self.parse_fulltext_target()?;
        self.expect(&CypherToken::On)?;
        if !self.eat_soft_word("EACH") {
            return Err(format!(
                "expected EACH after ON in CREATE FULLTEXT INDEX (write `ON EACH [n.prop, …]`), \
                 found {:?}",
                self.peek()
            ));
        }
        self.expect(&CypherToken::LBracket)?;
        let bound = match &target {
            FulltextTarget::Node { variable, .. }
            | FulltextTarget::Relationship { variable, .. } => variable.clone(),
        };
        let mut properties = Vec::new();
        loop {
            properties.push(self.parse_index_property_ref(bound.as_deref())?);
            if self.check(&CypherToken::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        self.expect(&CypherToken::RBracket)?;
        let options = self.parse_index_options()?;
        self.expect_statement_end("CREATE FULLTEXT INDEX")?;
        Ok(SchemaCommand::CreateFulltextIndex(CreateFulltextIndex {
            name,
            if_not_exists,
            target,
            properties,
            options,
        }))
    }

    /// `FOR (n:L) ON (n.p)` / `ON n.p` and `FOR ()-[r:T]-() …`, after the name
    /// and `IF NOT EXISTS` slots of `CREATE VECTOR INDEX`.
    pub(super) fn parse_vector_index_body(
        &mut self,
        name: Option<String>,
        if_not_exists: bool,
    ) -> Result<SchemaCommand, String> {
        let target = self.parse_ddl_for_target()?;
        self.expect(&CypherToken::On)?;
        if self.peek_soft_word("EACH") {
            return Err(
                "a vector index covers one property: write `ON (n.prop)`, not `ON EACH \
                        [...]`"
                    .to_string(),
            );
        }
        let parenthesised = self.check(&CypherToken::LParen);
        if parenthesised {
            self.advance();
        }
        let property = self.parse_index_property_ref(target.variable())?;
        if self.check(&CypherToken::Comma) {
            return Err(
                "a vector index covers exactly one property; a second property \
                        reference after ',' is not accepted"
                    .to_string(),
            );
        }
        if parenthesised {
            self.expect(&CypherToken::RParen)?;
        }
        let options = self.parse_index_options()?;
        self.expect_statement_end("CREATE VECTOR INDEX")?;
        Ok(SchemaCommand::CreateVectorIndex(CreateVectorIndex {
            name,
            if_not_exists,
            target,
            property,
            options,
        }))
    }

    /// `(n:A|B)` or `()-[r:T1|T2]-()`, consuming the `FOR`.
    fn parse_fulltext_target(&mut self) -> Result<FulltextTarget, String> {
        self.expect_soft_word("FOR", "full-text index pattern")?;
        self.expect(&CypherToken::LParen)?;
        if self.check(&CypherToken::RParen) {
            self.advance();
            if self.check(&CypherToken::LessThan) {
                self.advance();
            }
            self.expect(&CypherToken::Dash)?;
            self.expect(&CypherToken::LBracket)?;
            let variable = self.take_ddl_pattern_variable("relationship variable")?;
            self.expect(&CypherToken::Colon)?;
            let rel_types = self.parse_pipe_separated_names("relationship type after ':'")?;
            self.expect(&CypherToken::RBracket)?;
            self.expect(&CypherToken::Dash)?;
            if self.check(&CypherToken::GreaterThan) {
                self.advance();
            }
            self.expect(&CypherToken::LParen)?;
            self.expect(&CypherToken::RParen)?;
            return Ok(FulltextTarget::Relationship {
                variable,
                rel_types,
            });
        }
        let variable = self.take_ddl_pattern_variable("index variable")?;
        self.expect(&CypherToken::Colon)?;
        let labels = self.parse_pipe_separated_names("node label after ':'")?;
        self.expect(&CypherToken::RParen)?;
        Ok(FulltextTarget::Node { variable, labels })
    }

    /// `A|B|C`, at least one name.
    fn parse_pipe_separated_names(&mut self, context: &str) -> Result<Vec<String>, String> {
        let mut names = vec![self.expect_name(context)?];
        while self.check(&CypherToken::Pipe) {
            self.advance();
            names.push(self.expect_name(context)?);
        }
        Ok(names)
    }

    /// `n.prop`, checked against the pattern variable when there is one.
    fn parse_index_property_ref(&mut self, bound: Option<&str>) -> Result<String, String> {
        let prefix = self.expect_name("property reference like n.prop")?;
        self.expect(&CypherToken::Dot)?;
        let property = self.expect_name("property name after '.'")?;
        if let Some(bound) = bound {
            if prefix != bound {
                return Err(format!(
                    "property reference '{prefix}.{property}' does not use the variable \
                     '{bound}' bound by the FOR pattern"
                ));
            }
        }
        Ok(property)
    }

    /// `OPTIONS { indexConfig: { `key`: literal, … } [, indexProvider: 'x'] }`;
    /// empty when the statement has none.
    fn parse_index_options(&mut self) -> Result<IndexOptions, String> {
        let mut options = IndexOptions::default();
        if !self.eat_soft_word("OPTIONS") {
            return Ok(options);
        }
        self.expect(&CypherToken::LBrace)?;
        while !self.check(&CypherToken::RBrace) {
            let key = self.expect_name("an OPTIONS key (indexConfig or indexProvider)")?;
            self.expect(&CypherToken::Colon)?;
            if key.eq_ignore_ascii_case("indexConfig") {
                options.index_config = self.parse_index_config_map()?;
            } else if key.eq_ignore_ascii_case("indexProvider") {
                match self.advance().cloned() {
                    Some(CypherToken::StringLit(provider)) => {
                        options.index_provider = Some(provider)
                    }
                    other => {
                        return Err(format!(
                            "indexProvider must be a string literal, found {other:?}"
                        ))
                    }
                }
            } else {
                return Err(format!(
                    "unknown OPTIONS key '{key}': the accepted keys are indexConfig and \
                     indexProvider"
                ));
            }
            if self.check(&CypherToken::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        self.expect(&CypherToken::RBrace)?;
        Ok(options)
    }

    /// `{ `key`: literal, … }`, rejecting a key written twice.
    fn parse_index_config_map(&mut self) -> Result<Vec<(String, DdlOptionValue)>, String> {
        self.expect(&CypherToken::LBrace)?;
        let mut entries: Vec<(String, DdlOptionValue)> = Vec::new();
        while !self.check(&CypherToken::RBrace) {
            let key = self.expect_name("an indexConfig key")?;
            self.expect(&CypherToken::Colon)?;
            let value = self.parse_option_literal(&key)?;
            if entries.iter().any(|(existing, _)| *existing == key) {
                return Err(format!("indexConfig key '{key}' is given twice"));
            }
            entries.push((key, value));
            if self.check(&CypherToken::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        self.expect(&CypherToken::RBrace)?;
        Ok(entries)
    }

    fn parse_option_literal(&mut self, key: &str) -> Result<DdlOptionValue, String> {
        let negative = self.check(&CypherToken::Dash);
        if negative {
            self.advance();
        }
        match (self.advance().cloned(), negative) {
            (Some(CypherToken::IntLit(n)), neg) => {
                Ok(DdlOptionValue::Int(if neg { -n } else { n }))
            }
            (Some(CypherToken::FloatLit(f)), neg) => {
                Ok(DdlOptionValue::Float(if neg { -f } else { f }))
            }
            (Some(CypherToken::StringLit(s)), false) => Ok(DdlOptionValue::Str(s)),
            (Some(CypherToken::True), false) => Ok(DdlOptionValue::Bool(true)),
            (Some(CypherToken::False), false) => Ok(DdlOptionValue::Bool(false)),
            (other, _) => Err(format!(
                "indexConfig value for '{key}' must be a number, string or boolean literal, \
                 found {other:?}"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::parse_cypher;
    use super::*;

    fn command(input: &str) -> SchemaCommand {
        let query =
            parse_cypher(input).unwrap_or_else(|e| panic!("`{input}` failed to parse: {e}"));
        match query.clauses.into_iter().next().unwrap() {
            Clause::Schema(command) => command,
            other => panic!("`{input}` parsed as {other:?}"),
        }
    }

    fn fulltext(input: &str) -> CreateFulltextIndex {
        match command(input) {
            SchemaCommand::CreateFulltextIndex(create) => create,
            other => panic!("`{input}` parsed as {other:?}"),
        }
    }

    fn vector(input: &str) -> CreateVectorIndex {
        match command(input) {
            SchemaCommand::CreateVectorIndex(create) => create,
            other => panic!("`{input}` parsed as {other:?}"),
        }
    }

    fn error(input: &str) -> String {
        parse_cypher(input)
            .map(|q| panic!("`{input}` unexpectedly parsed to {:?}", q.clauses))
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn graphbridge_fulltext_form_parses() {
        let create = fulltext("CREATE FULLTEXT INDEX `f` FOR (n:`Doc`) ON EACH [n.`title`]");
        assert_eq!(create.name.as_deref(), Some("f"));
        assert!(!create.if_not_exists);
        assert_eq!(
            create.target,
            FulltextTarget::Node {
                variable: Some("n".to_string()),
                labels: vec!["Doc".to_string()],
            }
        );
        assert_eq!(create.properties, vec!["title".to_string()]);
        assert_eq!(create.options, IndexOptions::default());
    }

    #[test]
    fn multi_label_multi_property_and_if_not_exists() {
        let create =
            fulltext("CREATE FULLTEXT INDEX docs IF NOT EXISTS FOR (n:A|B|C) ON EACH [n.p, n.q]");
        assert!(create.if_not_exists);
        match create.target {
            FulltextTarget::Node { labels, .. } => assert_eq!(labels, ["A", "B", "C"]),
            other => panic!("{other:?}"),
        }
        assert_eq!(create.properties, ["p", "q"]);
    }

    #[test]
    fn unnamed_fulltext_index_has_no_name() {
        let create = fulltext("CREATE FULLTEXT INDEX FOR (n:Doc) ON EACH [n.title]");
        assert_eq!(create.name, None);
    }

    #[test]
    fn relationship_fulltext_form_parses_in_every_direction() {
        for input in [
            "CREATE FULLTEXT INDEX r FOR ()-[r:T1|T2]-() ON EACH [r.p]",
            "CREATE FULLTEXT INDEX r FOR ()-[r:T1|T2]->() ON EACH [r.p]",
            "CREATE FULLTEXT INDEX r FOR ()<-[r:T1|T2]-() ON EACH [r.p]",
        ] {
            let create = fulltext(input);
            assert_eq!(
                create.target,
                FulltextTarget::Relationship {
                    variable: Some("r".to_string()),
                    rel_types: vec!["T1".to_string(), "T2".to_string()],
                },
                "for `{input}`"
            );
        }
    }

    #[test]
    fn fulltext_options_keep_backtick_quoted_keys() {
        let create = fulltext(
            "CREATE FULLTEXT INDEX f FOR (n:Doc) ON EACH [n.t] OPTIONS {indexConfig: \
             {`fulltext.analyzer`: 'standard-no-stop-words', \
             `fulltext.eventually_consistent`: true}}",
        );
        assert_eq!(
            create.options.config("fulltext.analyzer"),
            Some(&DdlOptionValue::Str("standard-no-stop-words".to_string()))
        );
        assert_eq!(
            create.options.config("fulltext.eventually_consistent"),
            Some(&DdlOptionValue::Bool(true))
        );
    }

    #[test]
    fn graphbridge_vector_form_parses() {
        let create = vector(
            "CREATE VECTOR INDEX `v` FOR (n:`Doc`) ON (n.`emb`) OPTIONS {indexConfig: \
             {`vector.dimensions`: 3, `vector.similarity_function`: 'cosine'}}",
        );
        assert_eq!(create.name.as_deref(), Some("v"));
        assert_eq!(
            create.target,
            DdlTarget::Node {
                variable: Some("n".to_string()),
                label: "Doc".to_string(),
            }
        );
        assert_eq!(create.property, "emb");
        assert_eq!(
            create.options.config("vector.dimensions"),
            Some(&DdlOptionValue::Int(3))
        );
        assert_eq!(
            create.options.config("vector.similarity_function"),
            Some(&DdlOptionValue::Str("cosine".to_string()))
        );
    }

    #[test]
    fn vector_accepts_unparenthesised_property_and_relationship_form() {
        let create = vector("CREATE VECTOR INDEX v IF NOT EXISTS FOR (n:Doc) ON n.emb");
        assert!(create.if_not_exists);
        assert_eq!(create.property, "emb");
        let rel = vector("CREATE VECTOR INDEX v FOR ()-[r:SIMILAR]-() ON (r.emb)");
        assert!(matches!(rel.target, DdlTarget::Relationship { .. }));
    }

    #[test]
    fn negative_and_float_option_values_parse() {
        let create = vector(
            "CREATE VECTOR INDEX v FOR (n:Doc) ON (n.e) OPTIONS {indexConfig: {a: -2, b: 0.5}}",
        );
        assert_eq!(create.options.config("a"), Some(&DdlOptionValue::Int(-2)));
        assert_eq!(
            create.options.config("b"),
            Some(&DdlOptionValue::Float(0.5))
        );
    }

    #[test]
    fn malformed_forms_report_the_problem_with_a_position() {
        for (input, needle) in [
            ("CREATE FULLTEXT INDEX f FOR (n:Doc) ON [n.t]", "EACH"),
            ("CREATE FULLTEXT INDEX f FOR (n:Doc) ON EACH []", "property reference"),
            ("CREATE FULLTEXT INDEX f FOR (n:Doc) ON EACH [m.t]", "does not use the variable"),
            ("CREATE FULLTEXT INDEX f FOR (n:Doc) ON EACH [n.t", "Expected"),
            ("CREATE FULLTEXT INDEX f FOR (n:A|) ON EACH [n.t]", "node label"),
            (
                "CREATE FULLTEXT INDEX f FOR (n:Doc) ON EACH [n.t] OPTIONS {bogus: 1}",
                "unknown OPTIONS key 'bogus'",
            ),
            (
                "CREATE FULLTEXT INDEX f FOR (n:Doc) ON EACH [n.t] OPTIONS {indexConfig: {a: b}}",
                "must be a number, string or boolean",
            ),
            (
                "CREATE FULLTEXT INDEX f FOR (n:Doc) ON EACH [n.t] OPTIONS {indexConfig: {a: 1, a: 2}}",
                "given twice",
            ),
            ("CREATE VECTOR INDEX v FOR (n:Doc) ON (n.a, n.b)", "exactly one property"),
            ("CREATE VECTOR INDEX v FOR (n:Doc) ON EACH [n.a]", "one property"),
            ("CREATE VECTOR INDEX v FOR (n:Doc) ON (n.a) RETURN 1", "unexpected token"),
        ] {
            let message = error(input);
            assert!(message.contains(needle), "for `{input}`: {message}");
            assert!(message.contains("line 1"), "no position for `{input}`: {message}");
        }
    }

    #[test]
    fn drop_index_parses_for_a_fulltext_name() {
        match command("DROP INDEX f IF EXISTS") {
            SchemaCommand::DropIndex(drop) => {
                assert_eq!(drop.selector, DropIndexSelector::Name("f".to_string()));
                assert!(drop.if_exists);
            }
            other => panic!("{other:?}"),
        }
    }
}
