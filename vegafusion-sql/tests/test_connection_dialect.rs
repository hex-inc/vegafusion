#[macro_use]
extern crate lazy_static;

use std::collections::HashMap;
use std::fs;
use std::str::FromStr;
use std::sync::Arc;
use tokio::runtime::Runtime;
use vegafusion_common::error::VegaFusionError;
use vegafusion_common::{data::table::VegaFusionTable, error::Result};
use vegafusion_dataframe::dataframe::DataFrame;
use vegafusion_sql::connection::{DummySqlConnection, SqlConnection};
use vegafusion_sql::dialect::Dialect;

#[cfg(feature = "datafusion-conn")]
use vegafusion_sql::connection::datafusion_conn::DataFusionConnection;

#[cfg(feature = "sqlite-conn")]
use vegafusion_sql::connection::sqlite_conn::SqLiteConnection;
use vegafusion_sql::dataframe::SqlDataFrame;

lazy_static! {
    pub static ref TOKIO_RUNTIME: Runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
}

fn check_dataframe_query(
    df_result: Result<Arc<dyn DataFrame>>,
    suite_name: &str,
    test_name: &str,
    dialect_name: &str,
    evaluable: bool,
) {
    let (expected_query, expected_table) =
        load_expected_query_and_result(suite_name, test_name, dialect_name);

    if expected_query == "UNSUPPORTED" {
        if let Err(VegaFusionError::SqlNotSupported(..)) = df_result {
            // expected, return successful
            println!("Unsupported");
            return;
        } else {
            panic!("Expected sort result to be an error")
        }
    }
    let df = df_result.unwrap();

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

#[cfg(test)]
mod test_values {
    use crate::*;
    use rstest::rstest;
    use serde_json::json;
    use vegafusion_common::data::table::VegaFusionTable;
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
    fn test_values1(dialect_name: &str) {
        println!("{dialect_name}");
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

        let df_result = SqlDataFrame::from_values(&table, conn);
        check_dataframe_query(df_result, "values", "values1", dialect_name, evaluable);
    }
}

#[cfg(test)]
mod test_sort {
    use crate::*;
    use datafusion_expr::{col, expr, Expr};
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
    fn test_default_null_ordering(dialect_name: &str) {
        println!("{dialect_name}");
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

        let df_result = df.sort(
            vec![
                Expr::Sort(expr::Sort {
                    expr: Box::new(col("a")),
                    asc: false,
                    nulls_first: false,
                }),
                Expr::Sort(expr::Sort {
                    expr: Box::new(col("c")),
                    asc: true,
                    nulls_first: true,
                }),
            ],
            None,
        );

        check_dataframe_query(
            df_result,
            "sort",
            "default_null_ordering",
            dialect_name,
            evaluable,
        );
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
        let sort_res = df.sort(
            vec![
                Expr::Sort(expr::Sort {
                    expr: Box::new(col("a")),
                    asc: false,
                    nulls_first: true,
                }),
                Expr::Sort(expr::Sort {
                    expr: Box::new(col("c")),
                    asc: true,
                    nulls_first: false,
                }),
            ],
            None,
        );

        check_dataframe_query(
            sort_res,
            "sort",
            "custom_null_ordering",
            dialect_name,
            evaluable,
        );
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
    fn test_order_with_limit(dialect_name: &str) {
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

        let df = df
            .sort(
                vec![
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
                ],
                Some(4),
            )
            .unwrap();
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
    fn test_limit1(dialect_name: &str) {
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

#[cfg(test)]
mod test_filter {
    use crate::*;
    use datafusion_expr::{col, expr, lit, Expr};
    use rstest::rstest;
    use serde_json::json;
    use std::ops::Add;
    use vegafusion_common::data::table::VegaFusionTable;
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
    fn test_simple_gte(dialect_name: &str) {
        println!("{dialect_name}");

        let (conn, evaluable) = TOKIO_RUNTIME.block_on(make_connection(dialect_name));

        let table = VegaFusionTable::from_json(
            &json!([
                {"a": 1, "b": 2, "c": "A"},
                {"a": 3, "b": 4, "c": "BB"},
                {"a": 5, "b": 6, "c": "CCC"},
                {"a": 7, "b": 8, "c": "DDDD"},
                {"a": 9, "b": 10, "c": "EEEEE"},
                {"a": 11, "b": 12, "c": "FFFFFF"},
            ]),
            1024,
        )
        .unwrap();

        let df = SqlDataFrame::from_values(&table, conn).unwrap();
        let df = df
            .filter((col("a").add(lit(2)).gt_eq(lit(9))).or(col("b").modulus(lit(4)).eq(lit(0))))
            .unwrap();
        let df_result = df.sort(
            vec![Expr::Sort(expr::Sort {
                expr: Box::new(col("a")),
                asc: true,
                nulls_first: true,
            })],
            None,
        );

        check_dataframe_query(df_result, "filter", "simple_gte", dialect_name, evaluable);
    }
}

#[cfg(test)]
mod test_aggregate {
    use crate::*;
    use datafusion_expr::{
        avg, col, count, expr, lit, max, min, round, sum, AggregateFunction, Expr,
    };
    use rstest::rstest;
    use serde_json::json;
    use std::ops::{Div, Mul};
    use vegafusion_common::data::table::VegaFusionTable;
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
    fn test_simple_aggs(dialect_name: &str) {
        println!("{dialect_name}");
        let (conn, evaluable) = TOKIO_RUNTIME.block_on(make_connection(dialect_name));

        let table = VegaFusionTable::from_json(
            &json!([
                {"a": 1, "b": 2, "c": "A"},
                {"a": 3, "b": 2, "c": "BB"},
                {"a": 5, "b": 3, "c": "CCC"},
                {"a": 7, "b": 3, "c": "DDDD"},
                {"a": 9, "b": 3, "c": "EEEEE"},
                {"a": 11, "b": 3, "c": "FFFFFF"},
            ]),
            1024,
        )
        .unwrap();

        let df = SqlDataFrame::from_values(&table, conn).unwrap();
        let df = df
            .aggregate(
                vec![col("b")],
                vec![
                    min(col("a")).alias("min_a"),
                    max(col("a")).alias("max_a"),
                    avg(col("a")).alias("avg_a"),
                    sum(col("a")).alias("sum_a"),
                    count(col("a")).alias("count_a"),
                ],
            )
            .unwrap();
        let df_result = df.sort(
            vec![Expr::Sort(expr::Sort {
                expr: Box::new(col("b")),
                asc: true,
                nulls_first: true,
            })],
            None,
        );

        check_dataframe_query(
            df_result,
            "aggregate",
            "simple_aggs",
            dialect_name,
            evaluable,
        );
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
    fn test_median_agg(dialect_name: &str) {
        println!("{dialect_name}");
        let (conn, evaluable) = TOKIO_RUNTIME.block_on(make_connection(dialect_name));

        let table = VegaFusionTable::from_json(
            &json!([
                {"a": 1, "b": 2},
                {"a": 3, "b": 2},
                {"a": 5.5, "b": 3},
                {"a": 7.5, "b": 3},
                {"a": 100, "b": 3},
            ]),
            1024,
        )
        .unwrap();

        let df = SqlDataFrame::from_values(&table, conn).unwrap();
        let df_result = df.aggregate(
            vec![],
            vec![
                count(col("a")).alias("count_a"),
                Expr::AggregateFunction(expr::AggregateFunction {
                    fun: AggregateFunction::Median,
                    args: vec![col("a")],
                    distinct: false,
                    filter: None,
                })
                .alias("median_a"),
            ],
        );

        check_dataframe_query(
            df_result,
            "aggregate",
            "median_agg",
            dialect_name,
            evaluable,
        );
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
    fn test_variance_aggs(dialect_name: &str) {
        println!("{dialect_name}");
        let (conn, evaluable) = TOKIO_RUNTIME.block_on(make_connection(dialect_name));

        let table = VegaFusionTable::from_json(
            &json!([
                {"a": 1, "b": 2},
                {"a": 3, "b": 2},
                {"a": 5, "b": 3},
                {"a": 7, "b": 3},
                {"a": 9, "b": 3},
            ]),
            1024,
        )
        .unwrap();

        let df = SqlDataFrame::from_values(&table, conn).unwrap();
        let df_result = df.aggregate(
            vec![col("b")],
            vec![
                round(
                    Expr::AggregateFunction(expr::AggregateFunction {
                        fun: AggregateFunction::Stddev,
                        args: vec![col("a")],
                        distinct: false,
                        filter: None,
                    })
                    .mul(lit(100)),
                )
                .div(lit(100))
                .alias("stddev_a"),
                round(
                    Expr::AggregateFunction(expr::AggregateFunction {
                        fun: AggregateFunction::StddevPop,
                        args: vec![col("a")],
                        distinct: false,
                        filter: None,
                    })
                    .mul(lit(100)),
                )
                .div(lit(100))
                .alias("stddev_pop_a"),
                round(
                    Expr::AggregateFunction(expr::AggregateFunction {
                        fun: AggregateFunction::Variance,
                        args: vec![col("a")],
                        distinct: false,
                        filter: None,
                    })
                    .mul(lit(100)),
                )
                .div(lit(100))
                .alias("var_a"),
                round(
                    Expr::AggregateFunction(expr::AggregateFunction {
                        fun: AggregateFunction::VariancePop,
                        args: vec![col("a")],
                        distinct: false,
                        filter: None,
                    })
                    .mul(lit(100)),
                )
                .div(lit(100))
                .alias("var_pop_a"),
            ],
        );
        let df_result = df_result.and_then(|df| {
            df.sort(
                vec![Expr::Sort(expr::Sort {
                    expr: Box::new(col("b")),
                    asc: true,
                    nulls_first: true,
                })],
                None,
            )
        });

        check_dataframe_query(
            df_result,
            "aggregate",
            "variance_aggs",
            dialect_name,
            evaluable,
        );
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
