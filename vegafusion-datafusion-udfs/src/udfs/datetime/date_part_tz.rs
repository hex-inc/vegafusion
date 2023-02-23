use std::sync::Arc;
use vegafusion_common::{
    arrow::datatypes::{DataType, TimeUnit},
    datafusion_expr::{
        ColumnarValue, ReturnTypeFunction, ScalarFunctionImplementation, ScalarUDF, Signature,
        Volatility,
    },
};

fn make_date_part_tz_udf() -> ScalarUDF {
    let scalar_fn: ScalarFunctionImplementation = Arc::new(move |_args: &[ColumnarValue]| {
        unimplemented!("date_part_tz function is not implemented by DataFusion")
    });

    let return_type: ReturnTypeFunction =
        Arc::new(move |_| Ok(Arc::new(DataType::Int32)));

    let signature = Signature::exact(
        vec![
            DataType::Utf8, // part
            DataType::Timestamp(TimeUnit::Millisecond, None),
            DataType::Utf8, // timezone
        ],
        Volatility::Immutable,
    );

    ScalarUDF::new("date_part_tz", &signature, &return_type, &scalar_fn)
}

lazy_static! {
    pub static ref DATE_PART_TZ_UDF: ScalarUDF = make_date_part_tz_udf();
}