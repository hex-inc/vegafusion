#[macro_use]
extern crate lazy_static;

use std::collections::HashMap;
use std::fs;
use std::str::FromStr;
use std::sync::Arc;
use tokio::runtime::Runtime;
use vegafusion_sql::connection::{DummySqlConnection, SqlConnection};
use vegafusion_sql::dialect::Dialect;

#[cfg(feature = "datafusion-conn")]
use vegafusion_sql::connection::datafusion_conn::DataFusionConnection;

#[cfg(feature = "sqlite-conn")]
use vegafusion_sql::connection::sqlite_conn::SqLiteConnection;

lazy_static! {
    pub static ref TOKIO_RUNTIME: Runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
}

#[cfg(test)]
mod test_values {
    use crate::*;
    use rstest::rstest;
    use serde_json::json;
    use vegafusion_common::data::table::VegaFusionTable;
    use vegafusion_dataframe::dataframe::DataFrame;
    use vegafusion_sql::dataframe::SqlDataFrame;

    #[rstest(
        dialect_name,
        case("athena"),
        case("bigquery"),
        case("clickhouse"),
        case("databricks"),
        case("datafusion"),
        case("dremio"),
        case("duckdb"),
        case("mysql"),
        case("postgres"),
        case("redshift"),
        case("snowflake"),
        case("sqlite")
    )]
    fn test(dialect_name: &str) {
        println!("{dialect_name}");
        let (expected_query, expected_table) =
            load_expected_query_and_result("values", "values1", dialect_name);

        let (conn, evaluable) = TOKIO_RUNTIME.block_on(make_connection(dialect_name));

        let table = VegaFusionTable::from_json(
            &json!([
                {"a": 1, "b": 2, "c": "A"},
                {"a": 3, "b": 4, "c": "BB"},
                {"a": 5, "b": 6, "c": "CCC"},
            ]),
            1024,
        )
        .unwrap();

        let df = SqlDataFrame::from_values(&table, conn).unwrap();
        let df = df.as_any().downcast_ref::<SqlDataFrame>().unwrap();

        let sql = df.as_query().to_string();
        println!("{sql}");
        assert_eq!(sql, expected_query);

        if evaluable {
            let table: VegaFusionTable = TOKIO_RUNTIME.block_on(df.collect()).unwrap();
            let table_str = table.pretty_format(None).unwrap();
            println!("{table_str}");
            assert_eq!(table_str, expected_table);
        }
    }
}

#[cfg(test)]
mod test_sort {
    use datafusion_expr::{col, Expr, expr};
    use crate::*;
    use rstest::rstest;
    use serde_json::json;
    use vegafusion_common::data::table::VegaFusionTable;
    use vegafusion_common::error::VegaFusionError;
    use vegafusion_dataframe::dataframe::DataFrame;
    use vegafusion_sql::dataframe::SqlDataFrame;

    #[rstest(
    dialect_name,
    case("athena"),
    case("bigquery"),
    case("clickhouse"),
    case("databricks"),
    case("datafusion"),
    case("dremio"),
    case("duckdb"),
    case("mysql"),
    case("postgres"),
    case("redshift"),
    case("snowflake"),
    case("sqlite")
    )]
    fn test_default_null_ordering(dialect_name: &str) {
        println!("{dialect_name}");
        let (expected_query, expected_table) =
            load_expected_query_and_result("sort", "default_null_ordering", dialect_name);

        let (conn, evaluable) = TOKIO_RUNTIME.block_on(make_connection(dialect_name));

        let table = VegaFusionTable::from_json(
            &json!([
                {"a": 1, "b": 4, "c": "BB"},
                {"a": 2, "b": 6, "c": "DDDD"},
                {"a": null, "b": 5, "c": "BB"},
                {"a": 2, "b": 7, "c": "CCC"},
                {"a": 1, "b": 8, "c": "CCC"},
                {"a": 1, "b": 2, "c": "A"},
            ]),
            1024,
        )
            .unwrap();

        let df = SqlDataFrame::from_values(&table, conn).unwrap();
        let df = df.as_any().downcast_ref::<SqlDataFrame>().unwrap();

        let df = df.sort(vec![
            Expr::Sort(expr::Sort {
                expr: Box::new(col("a")),
                asc: false,
                nulls_first: false,
            }),
            Expr::Sort(expr::Sort {
                expr: Box::new(col("c")),
                asc: true,
                nulls_first: true,
            })
        ], None).unwrap();
        let df = df.as_any().downcast_ref::<SqlDataFrame>().unwrap();

        let sql = df.as_query().to_string();
        println!("{sql}");
        assert_eq!(sql, expected_query);

        if evaluable {
            let table: VegaFusionTable = TOKIO_RUNTIME.block_on(df.collect()).unwrap();
            let table_str = table.pretty_format(None).unwrap();
            println!("{table_str}");
            assert_eq!(table_str, expected_table);
        }
    }

    #[rstest(
        dialect_name,
        case("athena"),
        case("bigquery"),
        case("clickhouse"),
        case("databricks"),
        case("datafusion"),
        case("dremio"),
        case("duckdb"),
        case("mysql"),
        case("postgres"),
        case("redshift"),
        case("snowflake"),
        case("sqlite")
    )]
    fn test_custom_null_ordering(dialect_name: &str) {
        println!("{dialect_name}");
        let (expected_query, expected_table) =
            load_expected_query_and_result("sort", "custom_null_ordering", dialect_name);

        let (conn, evaluable) = TOKIO_RUNTIME.block_on(make_connection(dialect_name));

        let table = VegaFusionTable::from_json(
            &json!([
                {"a": 1, "b": 4, "c": "BB"},
                {"a": 2, "b": 6, "c": "DDDD"},
                {"a": null, "b": 5, "c": "BB"},
                {"a": 2, "b": 7, "c": "CCC"},
                {"a": 1, "b": 8, "c": null},
                {"a": 1, "b": 2, "c": "A"},
            ]),
            1024,
        )
            .unwrap();

        let df = SqlDataFrame::from_values(&table, conn).unwrap();
        let df = df.as_any().downcast_ref::<SqlDataFrame>().unwrap();

        let sort_res  = df.sort(vec![
            Expr::Sort(expr::Sort {
                expr: Box::new(col("a")),
                asc: false,
                nulls_first: true,
            }),
            Expr::Sort(expr::Sort {
                expr: Box::new(col("c")),
                asc: true,
                nulls_first: false,
            })
        ], None);

        if expected_query == "UNSUPPORTED" {
            if let Err(VegaFusionError::SqlNotSupported(..)) = sort_res {
                // expected, return successful
                println!("Unsupported");
                return
            } else {
                panic!("Expected sort result to be an error")
            }
        }
        let df = sort_res.unwrap();
        let df = df.as_any().downcast_ref::<SqlDataFrame>().unwrap();

        let sql = df.as_query().to_string();
        println!("{sql}");
        assert_eq!(sql, expected_query);

        if evaluable {
            let table: VegaFusionTable = TOKIO_RUNTIME.block_on(df.collect()).unwrap();
            let table_str = table.pretty_format(None).unwrap();
            println!("{table_str}");
            assert_eq!(table_str, expected_table);
        }
    }

    #[rstest(
    dialect_name,
    case("athena"),
    case("bigquery"),
    case("clickhouse"),
    case("databricks"),
    case("datafusion"),
    case("dremio"),
    case("duckdb"),
    case("mysql"),
    case("postgres"),
    case("redshift"),
    case("snowflake"),
    case("sqlite")
    )]
    fn test_ordering_with_limit(dialect_name: &str) {
        println!("{dialect_name}");
        let (expected_query, expected_table) =
            load_expected_query_and_result("sort", "order_with_limit", dialect_name);

        let (conn, evaluable) = TOKIO_RUNTIME.block_on(make_connection(dialect_name));

        let table = VegaFusionTable::from_json(
            &json!([
                {"a": 1, "b": 4, "c": "BB"},
                {"a": 2, "b": 6, "c": "DDDD"},
                {"a": null, "b": 5, "c": "BB"},
                {"a": 4, "b": 7, "c": "CCC"},
                {"a": 5, "b": 8, "c": "CCC"},
                {"a": 6, "b": 2, "c": "A"},
            ]),
            1024,
        )
            .unwrap();

        let df = SqlDataFrame::from_values(&table, conn).unwrap();
        let df = df.as_any().downcast_ref::<SqlDataFrame>().unwrap();

        let df = df.sort(vec![
            Expr::Sort(expr::Sort {
                expr: Box::new(col("c")),
                asc: true,
                nulls_first: true,
            }),
            Expr::Sort(expr::Sort {
                expr: Box::new(col("b")),
                asc: true,
                nulls_first: true,
            }),
        ], Some(4)).unwrap();
        let df = df.as_any().downcast_ref::<SqlDataFrame>().unwrap();

        let sql = df.as_query().to_string();
        println!("{sql}");
        assert_eq!(sql, expected_query);

        if evaluable {
            let table: VegaFusionTable = TOKIO_RUNTIME.block_on(df.collect()).unwrap();
            let table_str = table.pretty_format(None).unwrap();
            println!("{table_str}");
            assert_eq!(table_str, expected_table);
        }
    }
}

#[cfg(test)]
mod test_limit {
    use crate::*;
    use rstest::rstest;
    use serde_json::json;
    use vegafusion_common::data::table::VegaFusionTable;
    use vegafusion_dataframe::dataframe::DataFrame;
    use vegafusion_sql::dataframe::SqlDataFrame;

    #[rstest(
    dialect_name,
    case("athena"),
    case("bigquery"),
    case("clickhouse"),
    case("databricks"),
    case("datafusion"),
    case("dremio"),
    case("duckdb"),
    case("mysql"),
    case("postgres"),
    case("redshift"),
    case("snowflake"),
    case("sqlite")
    )]
    fn test(dialect_name: &str) {
        println!("{dialect_name}");
        let (expected_query, expected_table) =
            load_expected_query_and_result("limit", "limit1", dialect_name);

        let (conn, evaluable) = TOKIO_RUNTIME.block_on(make_connection(dialect_name));

        let table = VegaFusionTable::from_json(
            &json!([
                {"a": 1, "b": 2, "c": "A"},
                {"a": 3, "b": 4, "c": "BB"},
                {"a": 5, "b": 6, "c": "CCC"},
                {"a": 7, "b": 8, "c": "DDDD"},
                {"a": 9, "b": 10, "c": "EEEEE"},
            ]),
            1024,
        )
            .unwrap();

        let df = SqlDataFrame::from_values(&table, conn).unwrap();
        let df = df.limit(3).unwrap();
        let df = df.as_any().downcast_ref::<SqlDataFrame>().unwrap();

        let sql = df.as_query().to_string();
        println!("{sql}");
        assert_eq!(sql, expected_query);

        if evaluable {
            let table: VegaFusionTable = TOKIO_RUNTIME.block_on(df.collect()).unwrap();
            let table_str = table.pretty_format(None).unwrap();
            println!("{table_str}");
            assert_eq!(table_str, expected_table);
        }
    }
}

async fn make_connection(name: &str) -> (Arc<dyn SqlConnection>, bool) {
    #[cfg(feature = "datafusion-conn")]
    if name == "datafusion" {
        return (Arc::new(DataFusionConnection::default()), true);
    }

    #[cfg(feature = "sqlite-conn")]
    if name == "sqlite" {
        let conn = SqLiteConnection::try_new("file::memory:?cache=shared")
            .await
            .unwrap();
        return (Arc::new(conn), true);
    }

    let dialect = Dialect::from_str(name).unwrap();
    (Arc::new(DummySqlConnection::new(dialect)), false)
}

// Utilities
fn crate_dir() -> String {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .display()
        .to_string()
}

fn load_expected_toml(name: &str) -> HashMap<String, HashMap<String, String>> {
    // Load spec
    let toml_path = format!("{}/tests/expected/{}.toml", crate_dir(), name);
    let toml_str = fs::read_to_string(toml_path).unwrap();
    toml::from_str(&toml_str).unwrap()
}

fn load_expected_query_and_result(
    suite_name: &str,
    test_name: &str,
    dialect_name: &str,
) -> (String, String) {
    let expected = load_expected_toml(suite_name);
    let expected = expected.get(test_name).unwrap();
    let expected_query = expected.get(dialect_name).unwrap().trim();
    let expected_table = expected.get("result").unwrap().trim();
    (expected_query.to_string(), expected_table.to_string())
}
