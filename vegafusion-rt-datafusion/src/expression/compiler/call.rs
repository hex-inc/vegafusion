/*
 * VegaFusion
 * Copyright (C) 2022 VegaFusion Technologies LLC
 *
 * This program is distributed under multiple licenses.
 * Please consult the license documentation provided alongside
 * this program the details of the active license.
 */
use crate::expression::compiler::array::array_constructor_udf;
use crate::expression::compiler::builtin_functions::array::length::make_length_udf;
use crate::expression::compiler::builtin_functions::array::span::make_span_udf;
use crate::expression::compiler::builtin_functions::control_flow::if_fn::if_fn;
use crate::expression::compiler::builtin_functions::date_time::datetime::{
    datetime_transform_fn, make_datetime_components_fn, to_date_transform, DATETIME_COMPONENTS,
};
use crate::expression::compiler::builtin_functions::math::isfinite::{
    is_finite_fn, make_is_finite_udf,
};
use crate::expression::compiler::builtin_functions::math::isnan::make_is_nan_udf;
use crate::expression::compiler::builtin_functions::math::pow::{make_pow_udf, POW_UDF};
use crate::expression::compiler::builtin_functions::type_checking::isvalid::is_valid_fn;
use crate::expression::compiler::compile;
use crate::expression::compiler::config::CompilationConfig;
use crate::expression::compiler::utils::cast_to;
use datafusion::arrow::datatypes::DataType;
use datafusion::logical_plan::{DFSchema, Expr};
use datafusion::physical_plan::udf::ScalarUDF;
use datafusion::prelude::SessionContext;
use datafusion_expr::BuiltinScalarFunction;
use std::collections::HashMap;
use std::ops::Deref;
use std::str::FromStr;
use std::sync::Arc;
use vegafusion_core::data::table::VegaFusionTable;
use vegafusion_core::error::{Result, ResultWithContext, VegaFusionError};
use vegafusion_core::proto::gen::expression::{
    expression, literal, CallExpression, Expression, Literal,
};

use crate::expression::compiler::builtin_functions::data::data_fn::data_fn;
use crate::expression::compiler::builtin_functions::data::vl_selection_resolve::vl_selection_resolve_fn;
use crate::expression::compiler::builtin_functions::data::vl_selection_test::vl_selection_test_fn;
use crate::expression::compiler::builtin_functions::date_time::date_format::{
    time_format_fn, utc_format_fn, FORMAT_TIMESTAMP_UDF,
};
use crate::expression::compiler::builtin_functions::date_time::date_parts::{
    DATE_TRANSFORM, HOUR_TRANSFORM, MINUTE_TRANSFORM, MONTH_TRANSFORM, SECOND_TRANSFORM,
    UTCDATE_TRANSFORM, UTCHOUR_TRANSFORM, UTCMINUTE_TRANSFORM, UTCMONTH_TRANSFORM,
    UTCSECOND_TRANSFORM, UTCYEAR_TRANSFORM, YEAR_TRANSFORM,
};
use crate::expression::compiler::builtin_functions::date_time::date_to_timestamptz::DATE_TO_TIMESTAMPTZ_UDF;
use crate::expression::compiler::builtin_functions::date_time::epoch_to_timestamptz::EPOCH_MS_TO_TIMESTAMPTZ_UDF;
use crate::expression::compiler::builtin_functions::date_time::str_to_timestamptz::STR_TO_TIMESTAMPTZ_UDF;
use crate::expression::compiler::builtin_functions::date_time::time::{
    time_fn, TIMESTAMPTZ_TO_EPOCH_MS,
};
use crate::expression::compiler::builtin_functions::date_time::timestamp_to_timestamptz::TIMESTAMP_TO_TIMESTAMPTZ_UDF;
use crate::expression::compiler::builtin_functions::date_time::timestamptz_to_timestamp::TIMESTAMPTZ_TO_TIMESTAMP_UDF;
use crate::expression::compiler::builtin_functions::type_checking::isdate::is_date_fn;
use crate::expression::compiler::builtin_functions::type_coercion::to_boolean::to_boolean_transform;
use crate::expression::compiler::builtin_functions::type_coercion::to_number::to_number_transform;
use crate::expression::compiler::builtin_functions::type_coercion::to_string::to_string_transform;
use crate::task_graph::timezone::RuntimeTzConfig;
use crate::transform::timeunit::{TIMEUNIT_END_UDF, TIMEUNIT_START_UDF};

pub type MacroFn = Arc<dyn Fn(&[Expression]) -> Result<Expression> + Send + Sync>;
pub type TransformFn = Arc<dyn Fn(&[Expr], &DFSchema) -> Result<Expr> + Send + Sync>;
pub type TzTransformFn =
    Arc<dyn Fn(&RuntimeTzConfig, &[Expr], &DFSchema) -> Result<Expr> + Send + Sync>;
pub type DataFn =
    Arc<dyn Fn(&VegaFusionTable, &[Expression], &DFSchema) -> Result<Expr> + Send + Sync>;

#[derive(Clone)]
pub enum VegaFusionCallable {
    /// A function that operates on the ESTree expression tree before compilation
    Macro(MacroFn),

    /// A function that operates on the compiled arguments and produces a new expression.
    Transform(TransformFn),

    /// A function that uses the local timezone to operate on the compiled arguments and
    /// produces a new expression.
    LocalTransform(TzTransformFn),

    /// A function that uses the UTC timezone to operate on the compiled arguments and
    /// produces a new expression.
    UtcTransform(TzTransformFn),

    /// Runtime function that is build in to DataFusion
    BuiltinScalarFunction {
        function: BuiltinScalarFunction,
        /// If Some, all arguments should be cast to provided type
        cast: Option<DataType>,
    },

    /// A custom runtime function that's not built into DataFusion
    ScalarUDF {
        udf: ScalarUDF,
        /// If Some, all arguments should be cast to provided type
        cast: Option<DataType>,
    },

    /// A custom macro that inputs a dataset, and uses that to generate the DataFusion Expr tree
    ///
    /// e.g. `data('brush')` or  `vlSelectionTest('brush', datum, true)`
    Data(DataFn),

    /// A custom runtime function that operates on a scale dataset
    ///
    /// Placeholder for now
    Scale,
}

pub fn compile_scalar_arguments(
    node: &CallExpression,
    config: &CompilationConfig,
    schema: &DFSchema,
    cast: &Option<DataType>,
) -> Result<Vec<Expr>> {
    let mut args: Vec<Expr> = Vec::new();
    for arg in &node.arguments {
        let compiled_arg = compile(arg, config, Some(schema))?;
        let arg_expr = match cast {
            None => compiled_arg,
            Some(dtype) => cast_to(compiled_arg, dtype, schema)?,
        };
        args.push(arg_expr);
    }
    Ok(args)
}

pub fn compile_call(
    node: &CallExpression,
    config: &CompilationConfig,
    schema: &DFSchema,
) -> Result<Expr> {
    let callable = config.callable_scope.get(&node.callee).ok_or_else(|| {
        VegaFusionError::compilation(&format!("No global function named {}", &node.callee))
    })?;

    match callable {
        VegaFusionCallable::Macro(callable) => {
            // Apply macro then recursively compile
            let new_expr = callable(&node.arguments)?;
            compile(&new_expr, config, Some(schema))
        }
        VegaFusionCallable::ScalarUDF { udf, cast } => {
            let args = compile_scalar_arguments(node, config, schema, cast)?;
            Ok(Expr::ScalarUDF {
                fun: Arc::new(udf.clone()),
                args,
            })
        }
        VegaFusionCallable::BuiltinScalarFunction { function, cast } => {
            let args = compile_scalar_arguments(node, config, schema, cast)?;
            Ok(Expr::ScalarFunction {
                fun: function.clone(),
                args,
            })
        }
        VegaFusionCallable::Data(callee) => {
            if let Some(v) = node.arguments.get(0) {
                match v.expr() {
                    expression::Expr::Literal(Literal {
                        value: Some(literal::Value::String(name)),
                        ..
                    }) => {
                        if let Some(dataset) = config.data_scope.get(name) {
                            callee(dataset, &node.arguments[1..], schema)
                        } else {
                            Err(VegaFusionError::internal(&format!(
                                "No dataset named {}. Available: {:?}",
                                name,
                                config.data_scope.keys()
                            )))
                        }
                    }
                    _ => Err(VegaFusionError::internal(&format!(
                        "The first argument to the {} function must be a literal \
                                string with the name of a dataset",
                        &node.callee
                    ))),
                }
            } else {
                Err(VegaFusionError::internal(&format!(
                    "The first argument to the {} function must be a literal \
                                string with the name of a dataset",
                    &node.callee
                )))
            }
        }
        VegaFusionCallable::Transform(callable) => {
            let args = compile_scalar_arguments(node, config, schema, &None)?;
            callable(&args, schema)
        }
        VegaFusionCallable::LocalTransform(callable) => {
            let args = compile_scalar_arguments(node, config, schema, &None)?;
            let tz_config = config
                .tz_config
                .with_context(|| "No local timezone info provided".to_string())?;
            callable(&tz_config, &args, schema)
        }
        VegaFusionCallable::UtcTransform(callable) => {
            let args = compile_scalar_arguments(node, config, schema, &None)?;
            let tz_config = RuntimeTzConfig {
                local_tz: chrono_tz::UTC,
                default_input_tz: chrono_tz::UTC,
            };
            callable(&tz_config, &args, schema)
        }
        _ => {
            todo!()
        }
    }
}

pub fn default_callables() -> HashMap<String, VegaFusionCallable> {
    let mut callables: HashMap<String, VegaFusionCallable> = HashMap::new();
    callables.insert("if".to_string(), VegaFusionCallable::Macro(Arc::new(if_fn)));

    // Numeric functions built into DataFusion with names that match Vega.
    // Cast arguments to Float64
    for fun_name in &[
        "abs", "acos", "asin", "atan", "ceil", "cos", "exp", "floor", "round", "sqrt", "tan",
    ] {
        let function = BuiltinScalarFunction::from_str(fun_name).unwrap();
        callables.insert(
            fun_name.to_string(),
            VegaFusionCallable::BuiltinScalarFunction {
                function,
                cast: Some(DataType::Float64),
            },
        );
    }

    // DataFusion ln is Vega log
    callables.insert(
        "log".to_string(),
        VegaFusionCallable::BuiltinScalarFunction {
            function: BuiltinScalarFunction::Ln,
            cast: Some(DataType::Float64),
        },
    );

    // Custom udfs
    callables.insert(
        "pow".to_string(),
        VegaFusionCallable::ScalarUDF {
            udf: make_pow_udf(),
            cast: Some(DataType::Float64),
        },
    );

    callables.insert(
        "isNaN".to_string(),
        VegaFusionCallable::ScalarUDF {
            udf: make_is_nan_udf(),
            cast: None,
        },
    );

    callables.insert(
        "isFinite".to_string(),
        VegaFusionCallable::Transform(Arc::new(is_finite_fn)),
    );

    callables.insert(
        "isValid".to_string(),
        VegaFusionCallable::Transform(Arc::new(is_valid_fn)),
    );

    callables.insert(
        "isDate".to_string(),
        VegaFusionCallable::Transform(Arc::new(is_date_fn)),
    );

    callables.insert(
        "length".to_string(),
        VegaFusionCallable::ScalarUDF {
            udf: make_length_udf(),
            cast: None,
        },
    );

    callables.insert(
        "span".to_string(),
        VegaFusionCallable::ScalarUDF {
            udf: make_span_udf(),
            cast: None,
        },
    );

    // Date parts
    callables.insert(
        "year".to_string(),
        VegaFusionCallable::LocalTransform(YEAR_TRANSFORM.deref().clone()),
    );
    // callables.insert(
    //     "quarter".to_string(),
    //     VegaFusionCallable::LocalTransform(QUARTER_TRANSFORM.deref().clone()),
    // );
    callables.insert(
        "month".to_string(),
        VegaFusionCallable::LocalTransform(MONTH_TRANSFORM.deref().clone()),
    );
    // callables.insert(
    //     "day".to_string(),
    //     VegaFusionCallable::LocalTransform(DAY_TRANSFORM.deref().clone()),
    // );
    callables.insert(
        "date".to_string(),
        VegaFusionCallable::LocalTransform(DATE_TRANSFORM.deref().clone()),
    );
    // callables.insert(
    //     "dayofyear".to_string(),
    //     VegaFusionCallable::LocalTransform(DAYOFYEAR_TRANSFORM.deref().clone()),
    // );
    callables.insert(
        "hours".to_string(),
        VegaFusionCallable::LocalTransform(HOUR_TRANSFORM.deref().clone()),
    );
    callables.insert(
        "minutes".to_string(),
        VegaFusionCallable::LocalTransform(MINUTE_TRANSFORM.deref().clone()),
    );
    callables.insert(
        "seconds".to_string(),
        VegaFusionCallable::LocalTransform(SECOND_TRANSFORM.deref().clone()),
    );
    // callables.insert(
    //     "milliseconds".to_string(),
    //     VegaFusionCallable::LocalTransform(MILLISECONDS_TRANSFORM.deref().clone()),
    // );

    // UTC
    callables.insert(
        "utcyear".to_string(),
        VegaFusionCallable::UtcTransform(UTCYEAR_TRANSFORM.deref().clone()),
    );
    // callables.insert(
    //     "utcquarter".to_string(),
    //     VegaFusionCallable::UtcTransform(UTCQUARTER_TRANSFORM.deref().clone()),
    // );
    callables.insert(
        "utcmonth".to_string(),
        VegaFusionCallable::UtcTransform(UTCMONTH_TRANSFORM.deref().clone()),
    );
    // callables.insert(
    //     "utcday".to_string(),
    //     VegaFusionCallable::UtcTransform(UTCDAY_TRANSFORM.deref().clone()),
    // );
    callables.insert(
        "utcdate".to_string(),
        VegaFusionCallable::UtcTransform(UTCDATE_TRANSFORM.deref().clone()),
    );
    // callables.insert(
    //     "utcdayofyear".to_string(),
    //     VegaFusionCallable::UtcTransform(UTCDAYOFYEAR_TRANSFORM.deref().clone()),
    // );
    callables.insert(
        "utchours".to_string(),
        VegaFusionCallable::UtcTransform(UTCHOUR_TRANSFORM.deref().clone()),
    );
    callables.insert(
        "utcminutes".to_string(),
        VegaFusionCallable::UtcTransform(UTCMINUTE_TRANSFORM.deref().clone()),
    );
    callables.insert(
        "utcseconds".to_string(),
        VegaFusionCallable::UtcTransform(UTCSECOND_TRANSFORM.deref().clone()),
    );
    // callables.insert(
    //     "utcmilliseconds".to_string(),
    //     VegaFusionCallable::UtcTransform(UTCMILLISECOND_TRANSFORM.deref().clone()),
    // );

    // date time
    callables.insert(
        "datetime".to_string(),
        VegaFusionCallable::LocalTransform(Arc::new(datetime_transform_fn)),
    );

    callables.insert(
        "utc".to_string(),
        VegaFusionCallable::UtcTransform(Arc::new(make_datetime_components_fn)),
    );

    callables.insert(
        "time".to_string(),
        VegaFusionCallable::LocalTransform(Arc::new(time_fn)),
    );
    callables.insert(
        "timeFormat".to_string(),
        VegaFusionCallable::LocalTransform(Arc::new(time_format_fn)),
    );
    callables.insert(
        "utcFormat".to_string(),
        VegaFusionCallable::LocalTransform(Arc::new(utc_format_fn)),
    );

    // coercion
    callables.insert(
        "toBoolean".to_string(),
        VegaFusionCallable::Transform(Arc::new(to_boolean_transform)),
    );
    callables.insert(
        "toDate".to_string(),
        VegaFusionCallable::LocalTransform(Arc::new(to_date_transform)),
    );
    callables.insert(
        "toNumber".to_string(),
        VegaFusionCallable::Transform(Arc::new(to_number_transform)),
    );
    callables.insert(
        "toString".to_string(),
        VegaFusionCallable::Transform(Arc::new(to_string_transform)),
    );

    // data
    callables.insert(
        "data".to_string(),
        VegaFusionCallable::Data(Arc::new(data_fn)),
    );

    callables.insert(
        "vlSelectionTest".to_string(),
        VegaFusionCallable::Data(Arc::new(vl_selection_test_fn)),
    );

    callables.insert(
        "vlSelectionResolve".to_string(),
        VegaFusionCallable::Data(Arc::new(vl_selection_resolve_fn)),
    );

    callables
}

pub fn make_session_context() -> SessionContext {
    let mut ctx = SessionContext::new();

    // isNan
    ctx.register_udf(make_is_nan_udf());

    // isFinite
    ctx.register_udf(make_is_finite_udf());

    // timeunit
    ctx.register_udf((*TIMEUNIT_START_UDF).clone());
    ctx.register_udf((*TIMEUNIT_END_UDF).clone());

    // datetime
    ctx.register_udf((*TIMESTAMP_TO_TIMESTAMPTZ_UDF).clone());
    ctx.register_udf((*TIMESTAMPTZ_TO_TIMESTAMP_UDF).clone());
    ctx.register_udf((*DATE_TO_TIMESTAMPTZ_UDF).clone());
    ctx.register_udf((*EPOCH_MS_TO_TIMESTAMPTZ_UDF).clone());
    ctx.register_udf((*STR_TO_TIMESTAMPTZ_UDF).clone());
    ctx.register_udf((*DATETIME_COMPONENTS).clone());
    ctx.register_udf((*TIMESTAMPTZ_TO_EPOCH_MS).clone());

    // timeformat
    ctx.register_udf((*FORMAT_TIMESTAMP_UDF).clone());

    // math
    ctx.register_udf((*POW_UDF).clone());

    // list
    ctx.register_udf(array_constructor_udf());
    ctx.register_udf(make_length_udf());

    ctx
}
