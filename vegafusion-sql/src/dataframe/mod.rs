use crate::compile::expr::ToSqlExpr;
use crate::compile::order::ToSqlOrderByExpr;
use crate::compile::select::ToSqlSelectItem;
use crate::connection::SqlConnection;
use crate::dialect::{Dialect, ValuesMode};
use arrow::datatypes::{Field, Schema, SchemaRef};
use async_trait::async_trait;
use datafusion_common::{Column, DFSchema, ScalarValue};
use datafusion_expr::{
    abs, col, expr, is_null, lit, max, min, when, window_function, AggregateFunction,
    BuiltInWindowFunction, BuiltinScalarFunction, Expr, ExprSchemable, WindowFrame,
    WindowFrameBound, WindowFrameUnits, WindowFunction,
};
use sqlparser::ast::{
    Cte, Expr as SqlExpr, Ident, Query, Select, SelectItem, SetExpr, Statement, TableAlias,
    TableFactor, TableWithJoins, Values, WildcardAdditionalOptions, With,
};
use sqlparser::parser::Parser;
use std::any::Any;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::ops::{Add, Div, Sub};
use std::sync::Arc;
use vegafusion_common::column::flat_col;
use vegafusion_common::data::table::VegaFusionTable;
use vegafusion_common::datatypes::to_numeric;
use vegafusion_common::error::{Result, ResultWithContext, VegaFusionError};
use vegafusion_dataframe::connection::Connection;
use vegafusion_dataframe::dataframe::{DataFrame, StackMode};

#[derive(Clone)]
pub struct SqlDataFrame {
    pub(crate) prefix: String,
    pub(crate) schema: SchemaRef,
    pub(crate) ctes: Vec<Query>,
    pub(crate) conn: Arc<dyn SqlConnection>,
}

#[async_trait]
impl DataFrame for SqlDataFrame {
    fn as_any(&self) -> &dyn Any {
        self as &dyn Any
    }

    fn schema(&self) -> Schema {
        self.schema.as_ref().clone()
    }

    fn connection(&self) -> Arc<dyn Connection> {
        self.conn.to_connection()
    }

    fn fingerprint(&self) -> u64 {
        let mut hasher = deterministic_hash::DeterministicHasher::new(DefaultHasher::new());

        // Add connection id in hash
        self.conn.id().hash(&mut hasher);

        // Add query to hash
        let query_str = self.as_query().to_string();
        query_str.hash(&mut hasher);

        hasher.finish()
    }

    async fn collect(&self) -> Result<VegaFusionTable> {
        let query_string = self.as_query().to_string();
        self.conn.fetch_query(&query_string, &self.schema).await
    }

    fn sort(&self, expr: Vec<Expr>, limit: Option<i32>) -> Result<Arc<dyn DataFrame>> {
        let mut query = self.make_select_star();
        let sql_exprs = expr
            .iter()
            .map(|expr| expr.to_sql_order(self.dialect(), &self.schema_df()?))
            .collect::<Result<Vec<_>>>()?;
        query.order_by = sql_exprs;
        if let Some(limit) = limit {
            query.limit = Some(
                lit(limit)
                    .to_sql(self.dialect(), &self.schema_df()?)
                    .unwrap(),
            )
        }
        self.chain_query(query, self.schema.as_ref().clone())
    }

    fn select(&self, expr: Vec<Expr>) -> Result<Arc<dyn DataFrame>> {
        let sql_expr_strs = expr
            .iter()
            .map(|expr| {
                Ok(expr
                    .to_sql_select(self.dialect(), &self.schema_df()?)?
                    .to_string())
            })
            .collect::<Result<Vec<_>>>()?;

        let select_csv = sql_expr_strs.join(", ");
        let query = parse_sql_query(
            &format!(
                "select {select_csv} from {parent}",
                select_csv = select_csv,
                parent = self.parent_name()
            ),
            self.dialect(),
        )?;

        // Build new schema
        let new_schema = make_new_schema_from_exprs(self.schema.as_ref(), expr.as_slice())?;

        self.chain_query(query, new_schema)
    }

    fn aggregate(&self, group_expr: Vec<Expr>, aggr_expr: Vec<Expr>) -> Result<Arc<dyn DataFrame>> {
        // Add group exprs to aggregates for SQL query
        let mut all_aggr_expr = aggr_expr;
        all_aggr_expr.extend(group_expr.clone());

        let sql_group_expr_strs = group_expr
            .iter()
            .map(|expr| Ok(expr.to_sql(self.dialect(), &self.schema_df()?)?.to_string()))
            .collect::<Result<Vec<_>>>()?;

        let sql_aggr_expr_strs = all_aggr_expr
            .iter()
            .map(|expr| {
                Ok(expr
                    .to_sql_select(self.dialect(), &self.schema_df()?)?
                    .to_string())
            })
            .collect::<Result<Vec<_>>>()?;

        let aggr_csv = sql_aggr_expr_strs.join(", ");

        let query = if sql_group_expr_strs.is_empty() {
            parse_sql_query(
                &format!(
                    "select {aggr_csv} from {parent}",
                    aggr_csv = aggr_csv,
                    parent = self.parent_name(),
                ),
                self.dialect(),
            )?
        } else {
            let group_by_csv = sql_group_expr_strs.join(", ");
            parse_sql_query(
                &format!(
                    "select {aggr_csv} from {parent} group by {group_by_csv}",
                    aggr_csv = aggr_csv,
                    parent = self.parent_name(),
                    group_by_csv = group_by_csv
                ),
                self.dialect(),
            )?
        };

        // Build new schema from aggregate expressions
        let new_schema =
            make_new_schema_from_exprs(self.schema.as_ref(), all_aggr_expr.as_slice())?;
        self.chain_query(query, new_schema)
    }

    fn joinaggregate(
        &self,
        group_expr: Vec<Expr>,
        aggr_expr: Vec<Expr>,
    ) -> Result<Arc<dyn DataFrame>> {
        let schema = self.schema_df()?;
        // let dialect = self.dialect();

        // Build csv str for new columns
        let inner_name = format!("{}_inner", self.parent_name());
        let new_col_names = aggr_expr
            .iter()
            .map(|col| Ok(col.display_name()?))
            .collect::<Result<HashSet<_>>>()?;

        // Build csv str of input columns
        let input_col_exprs = schema
            .fields()
            .iter()
            .filter_map(|field| {
                if new_col_names.contains(field.name()) {
                    None
                } else {
                    Some(flat_col(field.name()))
                }
            })
            .collect::<Vec<_>>();

        let new_col_strs = aggr_expr
            .iter()
            .map(|col| {
                let col = Expr::Column(Column {
                    relation: if self.dialect().joinaggregate_fully_qualified {
                        Some(inner_name.to_string())
                    } else {
                        None
                    },
                    name: col.display_name()?,
                })
                .alias(col.display_name()?);
                Ok(col
                    .to_sql_select(self.dialect(), &self.schema_df()?)?
                    .to_string())
            })
            .collect::<Result<Vec<_>>>()?;
        let new_col_csv = new_col_strs.join(", ");

        let schema_df = self.schema_df()?;
        let input_col_strs = schema
            .fields()
            .iter()
            .filter_map(|field| {
                if new_col_names.contains(field.name()) {
                    None
                } else {
                    let expr = Expr::Column(Column {
                        relation: if self.dialect().joinaggregate_fully_qualified {
                            Some(self.parent_name())
                        } else {
                            None
                        },
                        name: field.name().clone(),
                    })
                    .alias(field.name());
                    Some(
                        expr.to_sql_select(self.dialect(), &schema_df)
                            .unwrap()
                            .to_string(),
                    )
                }
            })
            .collect::<Vec<_>>();

        let input_col_csv = input_col_strs.join(", ");

        // Perform join aggregation
        let sql_group_expr_strs = group_expr
            .iter()
            .map(|expr| Ok(expr.to_sql(self.dialect(), &self.schema_df()?)?.to_string()))
            .collect::<Result<Vec<_>>>()?;

        let sql_aggr_expr_strs = aggr_expr
            .iter()
            .map(|expr| {
                Ok(expr
                    .to_sql_select(self.dialect(), &self.schema_df()?)?
                    .to_string())
            })
            .collect::<Result<Vec<_>>>()?;
        let aggr_csv = sql_aggr_expr_strs.join(", ");

        // Build new schema
        let mut new_schema_exprs = input_col_exprs;
        new_schema_exprs.extend(aggr_expr);
        let new_schema =
            make_new_schema_from_exprs(self.schema.as_ref(), new_schema_exprs.as_slice())?;

        if sql_group_expr_strs.is_empty() {
            self.chain_query_str(
                &format!(
                    "select {input_col_csv}, {new_col_csv} \
                    from {parent} \
                    CROSS JOIN (select {aggr_csv} from {parent}) as {inner_name}",
                    aggr_csv = aggr_csv,
                    parent = self.parent_name(),
                    input_col_csv = input_col_csv,
                    new_col_csv = new_col_csv,
                    inner_name = inner_name,
                ),
                new_schema,
            )
        } else {
            let group_by_csv = sql_group_expr_strs.join(", ");
            self.chain_query_str(
                &format!(
                    "select {input_col_csv}, {new_col_csv} \
                    from {parent} \
                    LEFT OUTER JOIN (select {aggr_csv}, {group_by_csv} from {parent} group by {group_by_csv}) as {inner_name} USING ({group_by_csv})",
                    aggr_csv = aggr_csv,
                    parent = self.parent_name(),
                    input_col_csv = input_col_csv,
                    new_col_csv = new_col_csv,
                    group_by_csv = group_by_csv,
                    inner_name = inner_name,
                ),
                new_schema
            )
        }
    }

    fn filter(&self, predicate: Expr) -> Result<Arc<dyn DataFrame>> {
        let sql_predicate = predicate.to_sql(self.dialect(), &self.schema_df()?)?;

        let query = parse_sql_query(
            &format!(
                "select * from {parent} where {sql_predicate}",
                parent = self.parent_name(),
                sql_predicate = sql_predicate,
            ),
            self.dialect(),
        )?;

        self.chain_query(query, self.schema.as_ref().clone())
            .with_context(|| format!("unsupported filter expression: {predicate}"))
    }

    fn limit(&self, limit: i32) -> Result<Arc<dyn DataFrame>> {
        let query = parse_sql_query(
            &format!(
                "select * from {parent} LIMIT {limit}",
                parent = self.parent_name(),
                limit = limit
            ),
            self.dialect(),
        )?;

        self.chain_query(query, self.schema.as_ref().clone())
            .with_context(|| "unsupported limit query".to_string())
    }

    fn fold(
        &self,
        fields: &[String],
        value_col: &str,
        key_col: &str,
        order_field: Option<&str>,
    ) -> Result<Arc<dyn DataFrame>> {
        // let dialect = self.dialect();

        // Build selection that includes all input fields that aren't shadowed by key/value cols
        let input_selection = self
            .schema()
            .fields()
            .iter()
            .filter_map(|f| {
                if f.name() == key_col || f.name() == value_col {
                    None
                } else {
                    Some(flat_col(f.name()))
                }
            })
            .collect::<Vec<_>>();

        // Build query per field
        let subquery_exprs = fields
            .iter()
            .enumerate()
            .map(|(i, field)| {
                // Clone input selection and add key/val cols to it
                let mut subquery_selection = input_selection.clone();
                subquery_selection.push(lit(field).alias(key_col));
                if self.schema().column_with_name(field).is_some() {
                    // Field exists as a column in the parent table
                    subquery_selection.push(flat_col(field).alias(value_col));
                } else {
                    // Field does not exist in parent table, fill in NULL instead
                    subquery_selection.push(lit(ScalarValue::Null).alias(value_col));
                }

                if let Some(order_field) = order_field {
                    let field_order_col = format!("{order_field}_field");
                    subquery_selection.push(lit(i as u32).alias(field_order_col));
                }
                Ok(subquery_selection)
            })
            .collect::<Result<Vec<_>>>()?;

        let subqueries = subquery_exprs
            .iter()
            .map(|subquery_selection| {
                // Create selection CSV for subquery
                let selection_strs = subquery_selection
                    .iter()
                    .map(|sel| {
                        Ok(sel
                            .to_sql_select(self.dialect(), &self.schema_df()?)?
                            .to_string())
                    })
                    .collect::<Result<Vec<_>>>()?;
                let selection_csv = selection_strs.join(", ");

                Ok(format!(
                    "SELECT {selection_csv} from {parent}",
                    selection_csv = selection_csv,
                    parent = self.parent_name()
                ))
            })
            .collect::<Result<Vec<_>>>()?;

        let union_subquery = subqueries.join(" UNION ALL ");
        let union_subquery_name = "_union";

        let mut selections = input_selection.clone();
        selections.push(flat_col(key_col));
        selections.push(flat_col(value_col));
        if let Some(order_field) = order_field {
            let field_order_col = format!("{order_field}_field");
            selections.push(flat_col(&field_order_col));
        }

        let selection_strs = selections
            .iter()
            .map(|sel| {
                Ok(sel
                    .to_sql_select(self.dialect(), &self.schema_df()?)?
                    .to_string())
            })
            .collect::<Result<Vec<_>>>()?;
        let selection_csv = selection_strs.join(", ");

        let sql =
            format!("SELECT {selection_csv} FROM ({union_subquery}) as {union_subquery_name}");

        let new_schmea =
            make_new_schema_from_exprs(self.schema.as_ref(), subquery_exprs[0].as_slice())?;
        let dataframe = self.chain_query_str(&sql, new_schmea)?;

        if let Some(order_field) = order_field {
            // Add new ordering column, ordering by:
            // 1. input row ordering
            // 2. field index
            let field_order_col = format!("{order_field}_field");
            let order_col = Expr::WindowFunction(expr::WindowFunction {
                fun: window_function::WindowFunction::BuiltInWindowFunction(
                    BuiltInWindowFunction::RowNumber,
                ),
                args: vec![],
                partition_by: vec![],
                order_by: vec![
                    Expr::Sort(expr::Sort {
                        expr: Box::new(flat_col(order_field)),
                        asc: true,
                        nulls_first: true,
                    }),
                    Expr::Sort(expr::Sort {
                        expr: Box::new(flat_col(&field_order_col)),
                        asc: true,
                        nulls_first: true,
                    }),
                ],
                window_frame: WindowFrame {
                    units: WindowFrameUnits::Rows,
                    start_bound: WindowFrameBound::Preceding(ScalarValue::UInt64(None)),
                    end_bound: WindowFrameBound::CurrentRow,
                },
            })
            .alias(order_field);

            // Build output selections
            let mut selections = input_selection;
            selections.push(flat_col(key_col));
            selections.push(flat_col(value_col));
            selections[0] = order_col;
            dataframe.select(selections)
        } else {
            Ok(dataframe)
        }
    }

    fn stack(
        &self,
        field: &str,
        orderby: Vec<Expr>,
        groupby: &[String],
        start_field: &str,
        stop_field: &str,
        mode: StackMode,
    ) -> Result<Arc<dyn DataFrame>> {
        // Save off input columns
        let input_fields: Vec<_> = self
            .schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect();

        // let dialect = self.dialect();

        // Build partitioning column expressions
        let partition_by: Vec<_> = groupby.iter().map(|group| flat_col(group)).collect();

        let numeric_field = Expr::ScalarFunction {
            fun: BuiltinScalarFunction::Coalesce,
            args: vec![to_numeric(flat_col(field), &self.schema_df()?)?, lit(0.0)],
        };

        if let StackMode::Zero = mode {
            // Build window expression
            let fun = WindowFunction::AggregateFunction(AggregateFunction::Sum);

            // Build window function to compute stacked value
            let window_expr = Expr::WindowFunction(expr::WindowFunction {
                fun,
                args: vec![numeric_field.clone()],
                partition_by,
                order_by: orderby,
                window_frame: WindowFrame {
                    units: WindowFrameUnits::Rows,
                    start_bound: WindowFrameBound::Preceding(ScalarValue::UInt64(None)),
                    end_bound: WindowFrameBound::CurrentRow,
                },
            })
            .alias(stop_field);

            let window_expr_str = window_expr
                .to_sql_select(self.dialect(), &self.schema_df()?)?
                .to_string();

            // For offset zero, we need to evaluate positive and negative field values separately,
            // then union the results. This is required to make sure stacks do not overlap. Negative
            // values stack in the negative direction and positive values stack in the positive
            // direction.
            let schema_exprs = vec![Expr::Wildcard, window_expr];
            let new_schema =
                make_new_schema_from_exprs(self.schema.as_ref(), schema_exprs.as_slice())?;

            let dataframe = self
                .chain_query_str(&format!(
                    "SELECT *, {window_expr_str} from {parent} WHERE {numeric_field} >= 0 UNION ALL \
                                    SELECT *, {window_expr_str} from {parent} WHERE {numeric_field} < 0",
                    parent = self.parent_name(),
                    window_expr_str = window_expr_str,
                    numeric_field = numeric_field.to_sql(self.dialect(), &self.schema_df()?)?.to_string()
                ),
                                 new_schema)?;

            // Build final selection
            let mut final_selection: Vec<_> = input_fields
                .iter()
                .filter_map(|field| {
                    if field == start_field || field == stop_field {
                        None
                    } else {
                        Some(flat_col(field))
                    }
                })
                .collect();

            // Compute start column by adding numeric field to stop column
            let start_col = flat_col(stop_field).sub(numeric_field).alias(start_field);
            final_selection.push(start_col);
            final_selection.push(flat_col(stop_field));

            Ok(dataframe.select(final_selection.clone())?)
        } else {
            // Center or Normalized stack modes

            // take absolute value of numeric field
            let numeric_field = abs(numeric_field);

            // Create __stack column with numeric field
            let stack_col_name = "__stack";
            let dataframe =
                self.select(vec![Expr::Wildcard, numeric_field.alias(stack_col_name)])?;

            let dataframe = dataframe
                .as_any()
                .downcast_ref::<SqlDataFrame>()
                .unwrap()
                .clone();

            // Create aggregate for total of stack value
            let total_agg = Expr::AggregateFunction(expr::AggregateFunction {
                fun: AggregateFunction::Sum,
                args: vec![flat_col(stack_col_name)],
                distinct: false,
                filter: None,
            })
            .alias("__total");
            let total_agg_str = total_agg
                .to_sql_select(self.dialect(), &self.schema_df()?)?
                .to_string();

            // Add __total column with total or total per partition
            let schema_exprs = vec![Expr::Wildcard, total_agg];
            let new_schema =
                make_new_schema_from_exprs(&dataframe.schema(), schema_exprs.as_slice())?;

            let dataframe = if partition_by.is_empty() {
                dataframe.chain_query_str(
                    &format!(
                        "SELECT * from {parent} CROSS JOIN (SELECT {total_agg_str} from {parent})",
                        parent = dataframe.parent_name(),
                        total_agg_str = total_agg_str,
                    ),
                    new_schema,
                )?
            } else {
                let partition_by_strs = partition_by
                    .iter()
                    .map(|p| Ok(p.to_sql(self.dialect(), &self.schema_df()?)?.to_string()))
                    .collect::<Result<Vec<_>>>()?;
                let partition_by_csv = partition_by_strs.join(", ");

                dataframe.chain_query_str(
                    &format!(
                        "SELECT * FROM {parent} INNER JOIN \
                        (SELECT {partition_by_csv}, {total_agg_str} from {parent} GROUP BY {partition_by_csv}) as __inner \
                        USING ({partition_by_csv})",
                        parent = dataframe.parent_name(),
                        partition_by_csv = partition_by_csv,
                        total_agg_str = total_agg_str,
                    ),
                    new_schema
                )?
            };

            // Build window function to compute cumulative sum of stack column
            let cumulative_field = "_cumulative";
            let fun = WindowFunction::AggregateFunction(AggregateFunction::Sum);
            let window_expr = Expr::WindowFunction(expr::WindowFunction {
                fun,
                args: vec![flat_col(stack_col_name)],
                partition_by,
                order_by: orderby,
                window_frame: WindowFrame {
                    units: WindowFrameUnits::Rows,
                    start_bound: WindowFrameBound::Preceding(ScalarValue::UInt64(None)),
                    end_bound: WindowFrameBound::CurrentRow,
                },
            })
            .alias(cumulative_field);

            // Perform selection to add new field value
            let dataframe = dataframe.select(vec![Expr::Wildcard, window_expr])?;

            // Build final_selection
            let mut final_selection: Vec<_> = input_fields
                .iter()
                .filter_map(|field| {
                    if field == start_field || field == stop_field {
                        None
                    } else {
                        Some(flat_col(field))
                    }
                })
                .collect();

            // Now compute stop_field column by adding numeric field to start_field
            let dataframe = match mode {
                StackMode::Center => {
                    let max_total = max(flat_col("__total")).alias("__max_total");
                    let max_total_str = max_total
                        .to_sql_select(self.dialect(), &self.schema_df()?)?
                        .to_string();

                    // Compute new schema
                    let schema_exprs = vec![Expr::Wildcard, max_total];
                    let new_schema =
                        make_new_schema_from_exprs(&dataframe.schema(), schema_exprs.as_slice())?;

                    let sqldataframe = dataframe
                        .as_any()
                        .downcast_ref::<SqlDataFrame>()
                        .unwrap()
                        .clone();
                    let dataframe = sqldataframe
                        .chain_query_str(
                            &format!(
                                "SELECT * from {parent} CROSS JOIN (SELECT {max_total_str} from {parent}) as _cross",
                                parent = sqldataframe.parent_name(),
                                max_total_str = max_total_str,
                            ),
                            new_schema
                        )?;

                    let first = flat_col("__max_total")
                        .sub(flat_col("__total"))
                        .div(lit(2.0));
                    let first_col = flat_col(cumulative_field).add(first);
                    let stop_col = first_col.clone().alias(stop_field);
                    let start_col = first_col.sub(flat_col(stack_col_name)).alias(start_field);
                    final_selection.push(start_col);
                    final_selection.push(stop_col);

                    dataframe
                }
                StackMode::Normalize => {
                    let total_zero = flat_col("__total").eq(lit(0.0));

                    let start_col = when(total_zero.clone(), lit(0.0))
                        .otherwise(
                            flat_col(cumulative_field)
                                .sub(flat_col(stack_col_name))
                                .div(flat_col("__total")),
                        )?
                        .alias(start_field);

                    final_selection.push(start_col);

                    let stop_col = when(total_zero, lit(0.0))
                        .otherwise(flat_col(cumulative_field).div(flat_col("__total")))?
                        .alias(stop_field);

                    final_selection.push(stop_col);

                    dataframe
                }
                _ => return Err(VegaFusionError::internal("Unexpected stack mode")),
            };

            Ok(dataframe.select(final_selection.clone())?)
        }
    }

    fn impute(
        &self,
        field: &str,
        value: ScalarValue,
        key: &str,
        groupby: &[String],
        order_field: Option<&str>,
    ) -> Result<Arc<dyn DataFrame>> {
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

            self.select(select_columns)
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
            let key_col_str = key_col
                .to_sql_select(self.dialect(), &self.schema_df()?)?
                .to_string();

            let group_cols = groupby.iter().map(|c| flat_col(c)).collect::<Vec<_>>();
            let group_col_strs = group_cols
                .iter()
                .map(|c| {
                    Ok(c.to_sql_select(self.dialect(), &self.schema_df()?)?
                        .to_string())
                })
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

            let select_column_strs: Vec<_> = if self.dialect().impute_fully_qualified {
                // Some dialects (e.g. Clickhouse) require that references to columns in nested
                // subqueries be qualified with the subquery alias. Other dialects don't support this
                original_columns
                    .iter()
                    .map(|col_name| {
                        let expr = if col_name == field {
                            Expr::ScalarFunction {
                                fun: BuiltinScalarFunction::Coalesce,
                                args: vec![flat_col(field), lit(value.clone())],
                            }
                            .alias(col_name)
                        } else if col_name == key {
                            Expr::Column(Column {
                                relation: Some("_key".to_string()),
                                name: col_name.clone(),
                            })
                            .alias(col_name)
                        } else if groupby.contains(col_name) {
                            Expr::Column(Column {
                                relation: Some("_groups".to_string()),
                                name: col_name.clone(),
                            })
                            .alias(col_name)
                        } else {
                            flat_col(col_name)
                        };
                        Ok(expr
                            .to_sql_select(self.dialect(), &self.schema_df()?)?
                            .to_string())
                    })
                    .collect::<Result<Vec<_>>>()?
            } else {
                select_columns
                    .iter()
                    .map(|c| {
                        Ok(c.to_sql_select(self.dialect(), &self.schema_df()?)?
                            .to_string())
                    })
                    .collect::<Result<Vec<_>>>()?
            };

            let select_column_csv = select_column_strs.join(", ");

            let mut using_strs = vec![key_col_str.clone()];
            using_strs.extend(group_col_strs);
            let using_csv = using_strs.join(", ");

            if let Some(order_field) = order_field {
                // Query with ordering column
                let sql = format!(
                    "SELECT {select_column_csv}, {order_key_col}, {order_group_col} \
                     FROM (SELECT {key}, min({order_col}) as {order_key_col} from {parent} WHERE {key} IS NOT NULL GROUP BY {key}) AS _key \
                     CROSS JOIN (SELECT {group_cols_csv}, min({order_col}) as {order_group_col} from {parent} GROUP BY {group_cols_csv}) as _groups \
                     LEFT OUTER JOIN {parent} \
                     USING ({using_csv})",
                    select_column_csv = select_column_csv,
                    group_cols_csv = group_cols_csv,
                    key = key_col_str,
                    using_csv = using_csv,
                    order_col = col(order_field).to_sql(self.dialect(), &self.schema_df()?)?.to_string(),
                    order_group_col = col(format!("{order_field}_groups")).to_sql(self.dialect(), &self.schema_df()?)?.to_string(),
                    order_key_col = col(format!("{order_field}_key")).to_sql(self.dialect(), &self.schema_df()?)?.to_string(),
                    parent = self.parent_name(),
                );

                let mut schema_exprs = select_columns;
                schema_exprs.extend(vec![
                    min(flat_col(order_field)).alias(format!("{order_field}_key")),
                    min(flat_col(order_field)).alias(format!("{order_field}_groups")),
                ]);
                let new_schema =
                    make_new_schema_from_exprs(self.schema.as_ref(), schema_exprs.as_slice())?;
                let dataframe = self.chain_query_str(&sql, new_schema)?;

                // Override ordering column since null values may have been introduced in the query above.
                // Match input ordering with imputed rows (those will null ordering column) pushed
                // to the end.
                let order_by = if self.dialect().supports_null_ordering {
                    vec![
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
                            nulls_first: true,
                        }),
                        Expr::Sort(expr::Sort {
                            expr: Box::new(flat_col(&format!("{order_field}_key"))),
                            asc: true,
                            nulls_first: true,
                        }),
                    ]
                } else {
                    vec![
                        // Sort first by the original row order, pushing imputed rows to the end
                        Expr::Sort(expr::Sort {
                            expr: Box::new(is_null(flat_col(order_field))),
                            asc: true,
                            nulls_first: true,
                        }),
                        Expr::Sort(expr::Sort {
                            expr: Box::new(flat_col(order_field)),
                            asc: true,
                            nulls_first: true,
                        }),
                        // Sort imputed rows by first row that resides group
                        // then by first row that matches a key
                        Expr::Sort(expr::Sort {
                            expr: Box::new(flat_col(&format!("{order_field}_groups"))),
                            asc: true,
                            nulls_first: true,
                        }),
                        Expr::Sort(expr::Sort {
                            expr: Box::new(flat_col(&format!("{order_field}_key"))),
                            asc: true,
                            nulls_first: true,
                        }),
                    ]
                };

                let order_col = Expr::WindowFunction(expr::WindowFunction {
                    fun: window_function::WindowFunction::BuiltInWindowFunction(
                        BuiltInWindowFunction::RowNumber,
                    ),
                    args: vec![],
                    partition_by: vec![],
                    order_by,
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

                dataframe.select(selections)
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
                let new_schema =
                    make_new_schema_from_exprs(self.schema.as_ref(), select_columns.as_slice())?;
                self.chain_query_str(&sql, new_schema)
            }
        }
    }
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

        let query = parse_sql_query(
            &format!("select {select_items} from {table}"),
            conn.dialect(),
        )?;

        Ok(Self {
            prefix: format!("{table}_"),
            ctes: vec![query],
            schema: Arc::new(schema),
            conn,
        })
    }

    pub fn from_values(
        values: &VegaFusionTable,
        conn: Arc<dyn SqlConnection>,
    ) -> Result<Arc<dyn DataFrame>> {
        let dialect = conn.dialect();
        let batch = values.to_record_batch()?;
        let schema = batch.schema();
        let schema_df = DFSchema::try_from(schema.as_ref().clone())?;

        let query = match &dialect.values_mode {
            ValuesMode::SelectUnion => {
                // Build query like
                //      SELECT 1 as a, 2 as b UNION ALL SELECT 3 as a, 4 as b;
                let mut expr_selects: Vec<Select> = Default::default();
                for r in 0..batch.num_rows() {
                    let mut projection: Vec<SelectItem> = Default::default();

                    for c in 0..batch.num_columns() {
                        let col = batch.column(c);
                        let df_value =
                            lit(ScalarValue::try_from_array(col, r)?).alias(schema.field(c).name());
                        let sql_value = df_value.to_sql_select(dialect, &schema_df)?;
                        projection.push(sql_value);
                    }

                    expr_selects.push(Select {
                        distinct: false,
                        top: None,
                        projection,
                        into: None,
                        selection: None,
                        from: Default::default(),
                        lateral_views: Default::default(),
                        group_by: Default::default(),
                        cluster_by: Default::default(),
                        distribute_by: Default::default(),
                        sort_by: Default::default(),
                        having: None,
                        qualify: None,
                    });
                }

                let select_strs = expr_selects
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>();
                let query_str = select_strs.join(" UNION ALL ");

                parse_sql_query(&query_str, &dialect)?
            }
            ValuesMode::ValuesWithSubqueryColumnAliases { explicit_row, .. }
            | ValuesMode::ValuesWithSelectColumnAliases { explicit_row, .. } => {
                // Build VALUES subquery
                let mut expr_rows: Vec<Vec<SqlExpr>> = Default::default();
                for r in 0..batch.num_rows() {
                    let mut expr_row: Vec<SqlExpr> = Default::default();
                    for c in 0..batch.num_columns() {
                        let col = batch.column(c);
                        let df_value = lit(ScalarValue::try_from_array(col, r)?);
                        let sql_value = df_value.to_sql(conn.dialect(), &schema_df)?;
                        expr_row.push(sql_value);
                    }
                    expr_rows.push(expr_row);
                }
                let values = Values {
                    explicit_row: *explicit_row,
                    rows: expr_rows,
                };
                let values_body = SetExpr::Values(values);
                let values_subquery = Query {
                    with: None,
                    body: Box::new(values_body),
                    order_by: Default::default(),
                    limit: None,
                    offset: None,
                    fetch: None,
                    locks: Default::default(),
                };

                let (projection, table_alias) = if let ValuesMode::ValuesWithSelectColumnAliases {
                    column_prefix,
                    base_index,
                    ..
                } = &dialect.values_mode
                {
                    let projection = schema
                        .fields
                        .iter()
                        .enumerate()
                        .map(|(i, field)| {
                            col(format!("{}{}", column_prefix, i + base_index))
                                .alias(field.name())
                                .to_sql_select(dialect, &schema_df)
                                .unwrap()
                        })
                        .collect::<Vec<_>>();

                    (projection, None)
                } else {
                    let projection = vec![SelectItem::Wildcard(WildcardAdditionalOptions {
                        opt_exclude: None,
                        opt_except: None,
                        opt_rename: None,
                    })];

                    let table_alias = TableAlias {
                        name: Ident {
                            value: "_values".to_string(),
                            quote_style: Some(dialect.quote_style),
                        },
                        columns: schema
                            .fields
                            .iter()
                            .map(|f| Ident {
                                value: f.name().clone(),
                                quote_style: Some(dialect.quote_style),
                            })
                            .collect(),
                    };

                    (projection, Some(table_alias))
                };

                let select_body = SetExpr::Select(Box::new(Select {
                    distinct: false,
                    top: None,
                    projection,
                    into: None,
                    from: vec![TableWithJoins {
                        relation: TableFactor::Derived {
                            lateral: false,
                            subquery: Box::new(values_subquery),
                            alias: table_alias,
                        },
                        joins: Vec::new(),
                    }],
                    lateral_views: Default::default(),
                    selection: None,
                    group_by: Default::default(),
                    cluster_by: Default::default(),
                    distribute_by: Default::default(),
                    sort_by: Default::default(),
                    having: None,
                    qualify: None,
                }));
                Query {
                    with: None,
                    body: Box::new(select_body),
                    order_by: Default::default(),
                    limit: None,
                    offset: None,
                    fetch: None,
                    locks: Default::default(),
                }
            }
        };

        Ok(Arc::new(SqlDataFrame {
            prefix: "values".to_string(),
            schema,
            ctes: vec![query],
            conn,
        }))
    }

    pub fn dialect(&self) -> &Dialect {
        self.conn.dialect()
    }

    pub fn parent_name(&self) -> String {
        parent_cte_name_for_index(&self.prefix, self.ctes.len())
    }

    pub fn as_query(&self) -> Query {
        query_chain_to_cte(self.ctes.as_slice(), &self.prefix)
    }

    fn chain_query_str(&self, query: &str, new_schema: Schema) -> Result<Arc<dyn DataFrame>> {
        // println!("chain_query_str: {}", query);
        let query_ast = parse_sql_query(query, &self.dialect())?;
        self.chain_query(query_ast, new_schema)
    }

    fn chain_query(&self, query: Query, new_schema: Schema) -> Result<Arc<dyn DataFrame>> {
        let mut new_ctes = self.ctes.clone();
        new_ctes.push(query);

        Ok(Arc::new(SqlDataFrame {
            prefix: self.prefix.clone(),
            schema: Arc::new(new_schema),
            ctes: new_ctes,
            conn: self.conn.clone(),
            // dialect: self.dialect.clone(),
        }))
    }

    fn make_select_star(&self) -> Query {
        parse_sql_query(
            &format!("select * from {parent}", parent = self.parent_name()),
            self.dialect(),
        )
        .unwrap()
    }
}

fn make_new_schema_from_exprs(schema: &Schema, exprs: &[Expr]) -> Result<Schema> {
    let mut fields: Vec<Field> = Vec::new();
    for expr in exprs {
        if let Expr::Wildcard = expr {
            // Add field for each input schema field
            fields.extend(schema.fields().clone())
        } else {
            // Add field for expression
            let schema_df = DFSchema::try_from(schema.clone())?;
            let dtype = expr.get_type(&schema_df)?;
            let name = expr.display_name()?;
            fields.push(Field::new(name, dtype, true));
        }
    }

    let new_schema = Schema::new(fields);
    Ok(new_schema)
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
                query: Box::new(query.clone()),
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

fn parse_sql_query(query: &str, dialect: &Dialect) -> Result<Query> {
    let statements: Vec<Statement> = Parser::parse_sql(dialect.parser_dialect().as_ref(), query)?;
    if let Some(statement) = statements.get(0) {
        if let Statement::Query(box_query) = statement {
            let query: &Query = box_query.as_ref();
            Ok(query.clone())
        } else {
            Err(VegaFusionError::internal("Parser result was not a query"))
        }
    } else {
        Err(VegaFusionError::internal("Parser result empty"))
    }
}