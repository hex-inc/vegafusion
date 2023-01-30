use crate::sql::compile::expr::ToSqlExpr;
use crate::sql::compile::order::ToSqlOrderByExpr;
use crate::sql::compile::select::ToSqlSelectItem;
use crate::sql::connection::SqlConnection;
use datafusion::common::DFSchema;
use datafusion::prelude::{Expr as DfExpr, SessionContext};
use datafusion_expr::{
    expr, lit, window_function, BuiltInWindowFunction, BuiltinScalarFunction, Expr, WindowFrame,
    WindowFrameBound, WindowFrameUnits,
};
use sqlgen::ast::Ident;
use sqlgen::ast::{Cte, With};
use sqlgen::ast::{Query, TableAlias};
use sqlgen::dialect::{Dialect, DialectDisplay};
use sqlgen::parser::Parser;
use std::collections::hash_map::DefaultHasher;

use crate::expression::escape::flat_col;
use crate::sql::connection::datafusion_conn::make_datafusion_dialect;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use vegafusion_core::arrow::datatypes::{Schema, SchemaRef};
use vegafusion_core::data::scalar::ScalarValue;
use vegafusion_core::data::table::VegaFusionTable;
use vegafusion_core::error::{Result, ResultWithContext, VegaFusionError};

#[derive(Clone)]
pub struct SqlDataFrame {
    prefix: String,
    schema: SchemaRef,
    ctes: Vec<Query>,
    conn: Arc<dyn SqlConnection>,
    session_context: Arc<SessionContext>,
    dialect: Arc<Dialect>,
}

impl SqlDataFrame {
    pub async fn try_new(conn: Arc<dyn SqlConnection>, table: &str) -> Result<Self> {
        let tables = conn.tables().await?;
        let schema = tables
            .get(table)
            .cloned()
            .with_context(|| format!("Connection has no table named {table}"))?;
        // Should quote column names
        let columns: Vec<_> = schema
            .fields()
            .iter()
            .map(|f| format!("\"{}\"", f.name()))
            .collect();
        let select_items = columns.join(", ");

        let query = Parser::parse_sql_query(&format!("select {select_items} from {table}"))?;

        Ok(Self {
            prefix: format!("{table}_"),
            ctes: vec![query],
            schema: Arc::new(schema.clone()),
            session_context: Arc::new(conn.session_context().await?),
            conn,
            dialect: Arc::new(make_datafusion_dialect()),
        })
    }

    pub fn schema(&self) -> Schema {
        self.schema.as_ref().clone()
    }

    pub fn schema_df(&self) -> DFSchema {
        DFSchema::try_from(self.schema.as_ref().clone()).unwrap()
    }

    pub fn dialect(&self) -> &Dialect {
        &self.dialect
    }

    pub fn fingerprint(&self) -> u64 {
        let mut hasher = deterministic_hash::DeterministicHasher::new(DefaultHasher::new());

        // Add connection id in hash
        self.conn.id().hash(&mut hasher);

        // Add query to hash
        let query_str = self.as_query().sql(self.conn.dialect()).unwrap();
        query_str.hash(&mut hasher);

        hasher.finish()
    }

    pub fn parent_name(&self) -> String {
        parent_cte_name_for_index(&self.prefix, self.ctes.len())
    }

    pub fn as_query(&self) -> Query {
        query_chain_to_cte(self.ctes.as_slice(), &self.prefix)
    }

    pub async fn chain_query_str(&self, query: &str) -> Result<Arc<Self>> {
        // println!("chain_query_str: {}", query);
        let query_ast = Parser::parse_sql_query(query)?;
        self.chain_query(query_ast).await
    }

    pub async fn chain_query(&self, query: Query) -> Result<Arc<Self>> {
        let mut new_ctes = self.ctes.clone();
        new_ctes.push(query);

        let combined_query = query_chain_to_cte(new_ctes.as_slice(), &self.prefix);

        // First, convert the combined query to a string using the connection's dialect to make
        // sure that it is supported by the connection
        combined_query
            .sql(self.conn.dialect())
            .map_err(|err| VegaFusionError::sql_not_supported(err.to_string()))?;

        // Now convert to string in the DataFusion dialect for schema inference
        let query_str = combined_query.sql(&self.dialect)?;
        // println!("datafusion: {}", query_str);

        let logical_plan = self
            .session_context
            .state()
            .create_logical_plan(&query_str)
            .await?;

        // println!("logical_plan: {:?}", logical_plan);
        let new_schema: Schema = logical_plan.schema().as_ref().into();

        Ok(Arc::new(SqlDataFrame {
            prefix: self.prefix.clone(),
            schema: Arc::new(new_schema),
            ctes: new_ctes,
            conn: self.conn.clone(),
            session_context: self.session_context.clone(),
            dialect: self.dialect.clone(),
        }))
    }

    pub async fn collect(&self) -> Result<VegaFusionTable> {
        let query_string = self.as_query().sql(self.conn.dialect())?;
        self.conn.fetch_query(&query_string, &self.schema).await
    }

    pub async fn sort(&self, expr: Vec<DfExpr>, limit: Option<i32>) -> Result<Arc<Self>> {
        let mut query = self.make_select_star();
        let sql_exprs = expr
            .iter()
            .map(|expr| expr.to_sql_order())
            .collect::<Result<Vec<_>>>()?;
        query.order_by = sql_exprs;
        if let Some(limit) = limit {
            query.limit = Some(lit(limit).to_sql().unwrap())
        }
        self.chain_query(query).await
    }

    pub async fn select(&self, expr: Vec<DfExpr>) -> Result<Arc<Self>> {
        let sql_expr_strs = expr
            .iter()
            .map(|expr| Ok(expr.to_sql_select()?.sql(&self.dialect)?))
            .collect::<Result<Vec<_>>>()?;

        let select_csv = sql_expr_strs.join(", ");
        let query = Parser::parse_sql_query(&format!(
            "select {select_csv} from {parent}",
            select_csv = select_csv,
            parent = self.parent_name()
        ))?;

        self.chain_query(query).await
    }

    pub async fn aggregate(
        &self,
        group_expr: Vec<DfExpr>,
        aggr_expr: Vec<DfExpr>,
    ) -> Result<Arc<Self>> {
        let sql_group_expr_strs = group_expr
            .iter()
            .map(|expr| Ok(expr.to_sql()?.sql(&self.dialect)?))
            .collect::<Result<Vec<_>>>()?;

        let mut sql_aggr_expr_strs = aggr_expr
            .iter()
            .map(|expr| Ok(expr.to_sql_select()?.sql(&self.dialect)?))
            .collect::<Result<Vec<_>>>()?;

        // Add group exprs to selection
        sql_aggr_expr_strs.extend(sql_group_expr_strs.clone());
        let aggr_csv = sql_aggr_expr_strs.join(", ");

        let query = if sql_group_expr_strs.is_empty() {
            Parser::parse_sql_query(&format!(
                "select {aggr_csv} from {parent}",
                aggr_csv = aggr_csv,
                parent = self.parent_name(),
            ))?
        } else {
            let group_by_csv = sql_group_expr_strs.join(", ");
            Parser::parse_sql_query(&format!(
                "select {aggr_csv} from {parent} group by {group_by_csv}",
                aggr_csv = aggr_csv,
                parent = self.parent_name(),
                group_by_csv = group_by_csv
            ))?
        };

        self.chain_query(query).await
    }

    pub async fn filter(&self, predicate: Expr) -> Result<Arc<Self>> {
        let sql_predicate = predicate.to_sql()?;

        let query = Parser::parse_sql_query(&format!(
            "select * from {parent} where {sql_predicate}",
            parent = self.parent_name(),
            sql_predicate = sql_predicate.sql(&self.dialect)?,
        ))?;

        self.chain_query(query)
            .await
            .with_context(|| format!("unsupported filter expression: {predicate}"))
    }

    pub async fn limit(&self, limit: i32) -> Result<Arc<Self>> {
        let query = Parser::parse_sql_query(&format!(
            "select * from {parent} LIMIT {limit}",
            parent = self.parent_name(),
            limit = limit
        ))?;

        self.chain_query(query)
            .await
            .with_context(|| "unsupported limit query".to_string())
    }

    pub async fn impute(
        &self,
        field: &str,
        value: ScalarValue,
        key: &str,
        groupby: &[String],
        order_field: Option<&str>,
    ) -> Result<Arc<Self>> {
        if groupby.is_empty() {
            // Value replacement for field with no groupby fields specified is equivalent to replacing
            // null values of that column with the fill value
            let select_columns: Vec<_> = self
                .schema()
                .fields()
                .iter()
                .map(|f| {
                    let col_name = f.name();
                    if col_name == field {
                        Expr::ScalarFunction {
                            fun: BuiltinScalarFunction::Coalesce,
                            args: vec![flat_col(field), lit(value.clone())],
                        }
                        .alias(col_name)
                    } else {
                        flat_col(col_name)
                    }
                })
                .collect();

            self.select(select_columns).await
        } else {
            // Save off names of columns in the original input DataFrame
            let original_columns: Vec<_> = self
                .schema()
                .fields()
                .iter()
                .map(|field| field.name().clone())
                .collect();

            // First step is to build up a new DataFrame that contains the all possible combinations
            // of the `key` and `groupby` columns
            let key_col = flat_col(key);
            let key_col_str = key_col.to_sql_select()?.sql(self.dialect())?;

            let group_cols = groupby.iter().map(|c| flat_col(c)).collect::<Vec<_>>();
            let group_col_strs = group_cols
                .iter()
                .map(|c| Ok(c.to_sql_select()?.sql(self.dialect())?))
                .collect::<Result<Vec<_>>>()?;
            let group_cols_csv = group_col_strs.join(", ");

            // Build final selection
            // Finally, select all of the original DataFrame columns, filling in missing values
            // of the `field` columns
            let select_columns: Vec<_> = original_columns
                .iter()
                .map(|col_name| {
                    if col_name == field {
                        Expr::ScalarFunction {
                            fun: BuiltinScalarFunction::Coalesce,
                            args: vec![flat_col(field), lit(value.clone())],
                        }
                        .alias(col_name)
                    } else {
                        flat_col(col_name)
                    }
                })
                .collect();

            let select_column_strs = select_columns
                .iter()
                .map(|c| Ok(c.to_sql_select()?.sql(self.dialect())?))
                .collect::<Result<Vec<_>>>()?;

            let select_column_csv = select_column_strs.join(", ");

            let mut using_strs = vec![key_col_str.clone()];
            using_strs.extend(group_col_strs.clone());
            let using_csv = using_strs.join(", ");

            if let Some(order_field) = order_field {
                // Query with ordering column
                let sql = format!(
                    "SELECT {select_column_csv}, {order_field}_key, {order_field}_groups \
                     FROM (SELECT {key}, min({order_field}) as {order_field}_key from {parent} WHERE {key} IS NOT NULL GROUP BY {key}) AS _key \
                     CROSS JOIN (SELECT {group_cols_csv}, min({order_field}) as {order_field}_groups from {parent} GROUP BY {group_cols_csv}) as _groups \
                     LEFT OUTER JOIN {parent} \
                     USING ({using_csv})",
                    select_column_csv = select_column_csv,
                    group_cols_csv = group_cols_csv,
                    key = key_col_str,
                    using_csv = using_csv,
                    order_field = order_field,
                    parent = self.parent_name(),
                );
                let dataframe = self.chain_query_str(&sql).await?;

                // Override ordering column since null values may have been introduced in the query above.
                // Match input ordering with imputed rows (those will null ordering column) pushed
                // to the end.
                let order_col = Expr::WindowFunction(expr::WindowFunction {
                    fun: window_function::WindowFunction::BuiltInWindowFunction(
                        BuiltInWindowFunction::RowNumber,
                    ),
                    args: vec![],
                    partition_by: vec![],
                    order_by: vec![
                        // Sort first by the original row order, pushing imputed rows to the end
                        Expr::Sort(expr::Sort {
                            expr: Box::new(flat_col(order_field)),
                            asc: true,
                            nulls_first: false,
                        }),
                        // Sort imputed rows by first row that resides group
                        // then by first row that matches a key
                        Expr::Sort(expr::Sort {
                            expr: Box::new(flat_col(&format!("{order_field}_groups"))),
                            asc: true,
                            nulls_first: false,
                        }),
                        Expr::Sort(expr::Sort {
                            expr: Box::new(flat_col(&format!("{order_field}_key"))),
                            asc: true,
                            nulls_first: false,
                        }),
                    ],
                    window_frame: WindowFrame {
                        units: WindowFrameUnits::Rows,
                        start_bound: WindowFrameBound::Preceding(ScalarValue::UInt64(None)),
                        end_bound: WindowFrameBound::CurrentRow,
                    },
                })
                .alias(order_field);

                // Build vector of selections
                let mut selections = dataframe
                    .schema()
                    .fields
                    .iter()
                    .filter_map(|field| {
                        if field.name().starts_with(order_field) {
                            None
                        } else {
                            Some(flat_col(field.name()))
                        }
                    })
                    .collect::<Vec<_>>();
                selections.insert(0, order_col);

                dataframe.select(selections).await
            } else {
                // Impute query without ordering column
                let sql = format!(
                    "SELECT {select_column_csv}  \
                     FROM (SELECT {key} from {parent} WHERE {key} IS NOT NULL GROUP BY {key}) AS _key \
                     CROSS JOIN (SELECT {group_cols_csv} from {parent} GROUP BY {group_cols_csv}) as _groups \
                     LEFT OUTER JOIN {parent} \
                     USING ({using_csv})",
                    select_column_csv = select_column_csv,
                    group_cols_csv = group_cols_csv,
                    key = key_col_str,
                    using_csv = using_csv,
                    parent = self.parent_name(),
                );
                self.chain_query_str(&sql).await
            }
        }
    }

    fn make_select_star(&self) -> Query {
        Parser::parse_sql_query(&format!(
            "select * from {parent}",
            parent = self.parent_name()
        ))
        .unwrap()
    }
}

fn cte_name_for_index(prefix: &str, index: usize) -> String {
    format!("{prefix}{index}")
}

fn parent_cte_name_for_index(prefix: &str, index: usize) -> String {
    cte_name_for_index(prefix, index - 1)
}

fn query_chain_to_cte(queries: &[Query], prefix: &str) -> Query {
    // Build vector of CTE AST nodes for all but the last query
    let cte_tables: Vec<_> = queries[..queries.len() - 1]
        .iter()
        .enumerate()
        .map(|(i, query)| {
            let this_cte_name = cte_name_for_index(prefix, i);
            Cte {
                alias: TableAlias {
                    name: Ident {
                        value: this_cte_name,
                        quote_style: None,
                    },
                    columns: vec![],
                },
                query: query.clone(),
                from: None,
            }
        })
        .collect();

    // The final query becomes the top-level query with CTEs attached to it
    let mut final_query = queries[queries.len() - 1].clone();
    final_query.with = if cte_tables.is_empty() {
        None
    } else {
        Some(With {
            recursive: false,
            cte_tables,
        })
    };

    final_query
}
