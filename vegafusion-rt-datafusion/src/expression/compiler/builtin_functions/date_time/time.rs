use datafusion::arrow::array::{ArrayRef, Date32Array, Int64Array};
use datafusion::arrow::compute::cast;
use datafusion::arrow::datatypes::{DataType, TimeUnit};
use datafusion::physical_plan::functions::{
    make_scalar_function, ReturnTypeFunction, Signature, Volatility,
};
use datafusion::physical_plan::udf::ScalarUDF;
use std::sync::Arc;
use vegafusion_core::arrow::compute::unary;

pub fn make_time_udf() -> ScalarUDF {
    let time_fn = move |args: &[ArrayRef]| {
        // Signature ensures there is a single argument
        let arg = &args[0];

        let arg = match arg.data_type() {
            DataType::Timestamp(TimeUnit::Millisecond, _) => cast(arg, &DataType::Int64)?,
            DataType::Date32 => {
                let ms_per_day = 1000 * 60 * 60 * 24_i64;
                let array = arg.as_any().downcast_ref::<Date32Array>().unwrap();

                let array: Int64Array = unary(array, |v| (v as i64) * ms_per_day);
                let array = Arc::new(array) as ArrayRef;
                cast(&array, &DataType::Int64)?
            }
            DataType::Date64 => cast(arg, &DataType::Int64)?,
            DataType::Int64 => arg.clone(),
            _ => panic!("Unexpected data type for date part function:"),
        };

        Ok(arg)
    };
    let time_fn = make_scalar_function(time_fn);

    let return_type: ReturnTypeFunction = Arc::new(move |_| Ok(Arc::new(DataType::Int64)));
    ScalarUDF::new(
        "time",
        &Signature::uniform(
            1,
            vec![
                DataType::Timestamp(TimeUnit::Millisecond, None),
                DataType::Date32,
                DataType::Date64,
                DataType::Int64,
            ],
            Volatility::Immutable,
        ),
        &return_type,
        &time_fn,
    )
}