//! Print the public API surface of schemadex-core as JSON. Run before
//! tagging v1.0 so the maintainer can review every locked-in symbol.

use schemadex_core::*;
use serde_json::json;

fn main() {
    // We can't introspect a crate's exports at runtime in stable Rust;
    // instead we hand-curate the list and assert each name exists at
    // compile time by reference. Update this list when adding or
    // removing a public re-export from lib.rs.
    let symbols: Vec<&'static str> = vec![
        "SchemaCache",
        "Database",
        "Table",
        "Column",
        "ForeignKey",
        "PrimaryKey",
        "DataType",
        "ColumnSample",
        "SampleStats",
        "SchemadexError",
        "Result",
        "ResolveResult",
        "SchemaIntrospector",
        "Backend",
        "QueryRunner",
        "QueryResult",
        "describe_for_agent",
        "DescribeOptions",
        "resolve_column",
        "render_table_for_agent",
        "validate_sql",
        "hint_for_error",
        "assert_readonly",
        "SynonymMap",
        "resolve_column_with_synonyms",
    ];

    // Compile-time pin: each line forces the compiler to verify the
    // symbol is reachable from the crate root with that name.
    let _: fn() -> Result<()> = || {
        let _ = SchemaCache::load;
        let _: Option<Database> = None;
        let _: Option<Table> = None;
        let _: Option<Column> = None;
        let _: Option<ForeignKey> = None;
        let _: Option<PrimaryKey> = None;
        let _: Option<DataType> = None;
        let _: Option<ColumnSample> = None;
        let _: Option<SampleStats> = None;
        let _: Option<SchemadexError> = None;
        let _: Option<ResolveResult> = None;
        let _: Option<Backend> = None;
        let _: Option<QueryResult> = None;
        let _: Option<DescribeOptions> = None;
        let _: Option<SynonymMap> = None;
        let _ = describe_for_agent;
        let _ = resolve_column;
        let _ = render_table_for_agent;
        let _ = validate_sql;
        let _ = hint_for_error;
        let _ = assert_readonly;
        let _ = resolve_column_with_synonyms;
        Ok(())
    };

    println!("{}", json!({
        "crate": "schemadex-core",
        "version": env!("CARGO_PKG_VERSION"),
        "symbol_count": symbols.len(),
        "symbols": symbols,
    }));
}
