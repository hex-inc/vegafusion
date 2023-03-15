// Licensed to the Apache Software Foundation (ASF) under one
// or more contributor license agreements.  See the NOTICE file
// distributed with this work for additional information
// regarding copyright ownership.  The ASF licenses this file
// to you under the Apache License, Version 2.0 (the
// "License"); you may not use this file except in compliance
// with the License.  You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing,
// software distributed under the License is distributed on an
// "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.  See the License for the
// specific language governing permissions and limitations
// under the License.

// ## VegaFusion notes
// -------------------
// This file was originally copied from Arrow (with license above) `json/writer.rs` with the
// following modifications.
//   1. Rather than skip writing null values, this version is updated to write the JSON
//      NULL value instead. This is needed for interoperability with Vega.
//   2. Date32, Date64, and Timestamp types are serialized as UTC milliseconds.

//! # JSON Writer
//!
//! This JSON writer converts Arrow [`RecordBatch`]es into arrays of
//! JSON objects or JSON formatted byte streams.
//!
//! ## Writing JSON Objects
//!
//! To serialize [`RecordBatch`]es into array of
//! [JSON](https://docs.serde.rs/serde_json/) objects, use
//! [`record_batches_to_json_rows`]:
//!

use std::iter;
use std::{fmt::Debug, io::Write};

use serde_json::map::Map as JsonMap;
use serde_json::Value;

use arrow::array::*;
use arrow::datatypes::*;
use arrow::error::{ArrowError, Result};
use arrow::json::JsonSerializable;
use arrow::record_batch::RecordBatch;

fn primitive_array_to_json<T>(array: &ArrayRef) -> Result<Vec<Value>>
where
    T: ArrowPrimitiveType,
    T::Native: JsonSerializable,
{
    Ok(as_primitive_array::<T>(array)
        .iter()
        .map(|maybe_value| match maybe_value {
            Some(v) => v.into_json_value().unwrap_or(Value::Null),
            None => Value::Null,
        })
        .collect())
}

fn struct_array_to_jsonmap_array(
    array: &StructArray,
    row_count: usize,
) -> Result<Vec<JsonMap<String, Value>>> {
    let inner_col_names = array.column_names();

    let mut inner_objs = iter::repeat(JsonMap::new())
        .take(row_count)
        .collect::<Vec<JsonMap<String, Value>>>();

    for (j, struct_col) in array.columns().iter().enumerate() {
        set_column_for_json_rows(&mut inner_objs, row_count, struct_col, inner_col_names[j])?
    }
    Ok(inner_objs)
}

/// Converts an arrow [`ArrayRef`] into a `Vec` of Serde JSON [`serde_json::Value`]'s
pub fn array_to_json_array(array: &ArrayRef) -> Result<Vec<Value>> {
    match array.data_type() {
        DataType::Null => Ok(iter::repeat(Value::Null).take(array.len()).collect()),
        DataType::Boolean => Ok(as_boolean_array(array)
            .iter()
            .map(|maybe_value| match maybe_value {
                Some(v) => v.into(),
                None => Value::Null,
            })
            .collect()),

        DataType::Utf8 => Ok(as_string_array(array)
            .iter()
            .map(|maybe_value| match maybe_value {
                Some(v) => v.into(),
                None => Value::Null,
            })
            .collect()),
        DataType::LargeUtf8 => Ok(as_largestring_array(array)
            .iter()
            .map(|maybe_value| match maybe_value {
                Some(v) => v.into(),
                None => Value::Null,
            })
            .collect()),
        DataType::Int8 => primitive_array_to_json::<Int8Type>(array),
        DataType::Int16 => primitive_array_to_json::<Int16Type>(array),
        DataType::Int32 => primitive_array_to_json::<Int32Type>(array),
        DataType::Int64 => primitive_array_to_json::<Int64Type>(array),
        DataType::UInt8 => primitive_array_to_json::<UInt8Type>(array),
        DataType::UInt16 => primitive_array_to_json::<UInt16Type>(array),
        DataType::UInt32 => primitive_array_to_json::<UInt32Type>(array),
        DataType::UInt64 => primitive_array_to_json::<UInt64Type>(array),
        DataType::Float32 => primitive_array_to_json::<Float32Type>(array),
        DataType::Float64 => primitive_array_to_json::<Float64Type>(array),
        DataType::List(_) => as_list_array(array)
            .iter()
            .map(|maybe_value| match maybe_value {
                Some(v) => Ok(Value::Array(array_to_json_array(&v)?)),
                None => Ok(Value::Null),
            })
            .collect(),
        DataType::LargeList(_) => as_large_list_array(array)
            .iter()
            .map(|maybe_value| match maybe_value {
                Some(v) => Ok(Value::Array(array_to_json_array(&v)?)),
                None => Ok(Value::Null),
            })
            .collect(),
        DataType::Struct(_) => {
            let jsonmaps = struct_array_to_jsonmap_array(as_struct_array(array), array.len())?;
            Ok(jsonmaps.into_iter().map(Value::Object).collect())
        }
        t => Err(ArrowError::JsonError(format!(
            "data type {t:?} not supported"
        ))),
    }
}
macro_rules! set_column_by_array_type {
    ($cast_fn:ident, $col_name:ident, $rows:ident, $array:ident, $row_count:ident) => {
        let arr = $cast_fn($array);
        $rows
            .iter_mut()
            .zip(arr.iter())
            .take($row_count)
            .for_each(|(row, maybe_value)| {
                if let Some(v) = maybe_value {
                    row.insert($col_name.to_string(), v.into());
                } else {
                    row.insert($col_name.to_string(), Value::Null);
                }
            });
    };
}

macro_rules! set_temporal_column_as_millis_by_array_type {
    ($array_type:ident, $col_name:ident, $rows:ident, $array:ident, $row_count:ident, $cast_fn:ident) => {
        let arr = $array.as_any().downcast_ref::<$array_type>().unwrap();

        $rows
            .iter_mut()
            .enumerate()
            .take($row_count)
            .for_each(|(i, row)| {
                if !arr.is_null(i) {
                    if let Some(v) = arr.$cast_fn(i) {
                        row.insert($col_name.to_string(), v.timestamp_millis().into());
                    } else {
                        row.insert($col_name.to_string(), Value::Null);
                    }
                } else {
                    row.insert($col_name.to_string(), Value::Null);
                }
            });
    };
}

macro_rules! set_temporal_column_by_array_type {
    ($array_type:ident, $col_name:ident, $rows:ident, $array:ident, $row_count:ident, $cast_fn:ident) => {
        let arr = $array.as_any().downcast_ref::<$array_type>().unwrap();

        $rows
            .iter_mut()
            .enumerate()
            .take($row_count)
            .for_each(|(i, row)| {
                if !arr.is_null(i) {
                    if let Some(v) = arr.$cast_fn(i) {
                        row.insert($col_name.to_string(), v.to_string().into());
                    } else {
                        row.insert($col_name.to_string(), Value::Null);
                    }
                } else {
                    row.insert($col_name.to_string(), Value::Null);
                }
            });
    };
}

fn set_column_by_primitive_type<T: ArrowPrimitiveType>(
    rows: &mut [JsonMap<String, Value>],
    row_count: usize,
    array: &ArrayRef,
    col_name: &str,
) where
    T: ArrowPrimitiveType,
    T::Native: JsonSerializable,
{
    let primitive_arr = as_primitive_array::<T>(array);

    rows.iter_mut()
        .zip(primitive_arr.iter())
        .take(row_count)
        .for_each(|(row, maybe_value)| {
            // when value is null, we simply skip setting the key
            if let Some(j) = maybe_value.and_then(|v| v.into_json_value()) {
                row.insert(col_name.to_string(), j);
            } else {
                row.insert(col_name.to_string(), Value::Null);
            }
        });
}

fn set_column_for_json_rows(
    rows: &mut [JsonMap<String, Value>],
    row_count: usize,
    array: &ArrayRef,
    col_name: &str,
) -> Result<()> {
    match array.data_type() {
        DataType::Int8 => {
            set_column_by_primitive_type::<Int8Type>(rows, row_count, array, col_name);
        }
        DataType::Int16 => {
            set_column_by_primitive_type::<Int16Type>(rows, row_count, array, col_name);
        }
        DataType::Int32 => {
            set_column_by_primitive_type::<Int32Type>(rows, row_count, array, col_name);
        }
        DataType::Int64 => {
            set_column_by_primitive_type::<Int64Type>(rows, row_count, array, col_name);
        }
        DataType::UInt8 => {
            set_column_by_primitive_type::<UInt8Type>(rows, row_count, array, col_name);
        }
        DataType::UInt16 => {
            set_column_by_primitive_type::<UInt16Type>(rows, row_count, array, col_name);
        }
        DataType::UInt32 => {
            set_column_by_primitive_type::<UInt32Type>(rows, row_count, array, col_name);
        }
        DataType::UInt64 => {
            set_column_by_primitive_type::<UInt64Type>(rows, row_count, array, col_name);
        }
        DataType::Float32 => {
            set_column_by_primitive_type::<Float32Type>(rows, row_count, array, col_name);
        }
        DataType::Float64 => {
            set_column_by_primitive_type::<Float64Type>(rows, row_count, array, col_name);
        }
        DataType::Null => {
            // when value is null, we still set the key
            rows.iter_mut().take(row_count).for_each(|row| {
                row.insert(col_name.to_string(), Value::Null);
            });
        }
        DataType::Boolean => {
            set_column_by_array_type!(as_boolean_array, col_name, rows, array, row_count);
        }
        DataType::Utf8 => {
            set_column_by_array_type!(as_string_array, col_name, rows, array, row_count);
        }
        DataType::Date32 => {
            // Write as integer UTC milliseconds
            let arr = array.as_any().downcast_ref::<Date32Array>().unwrap();
            rows.iter_mut()
                .enumerate()
                .take(row_count)
                .for_each(|(i, row)| {
                    if arr.is_valid(i) {
                        let days = arr.value(i) as i64;
                        let ms_per_day = 1000 * 60 * 60 * 24_i64;
                        let millis = days * ms_per_day;
                        row.insert(col_name.to_string(), millis.into());
                    } else {
                        row.insert(col_name.to_string(), Value::Null);
                    }
                });
        }
        DataType::Date64 => {
            // Write as integer UTC milliseconds
            let arr = array.as_any().downcast_ref::<Date64Array>().unwrap();
            rows.iter_mut()
                .enumerate()
                .take(row_count)
                .for_each(|(i, row)| {
                    if arr.is_valid(i) {
                        let millis = arr.value(i);
                        row.insert(col_name.to_string(), millis.into());
                    } else {
                        row.insert(col_name.to_string(), Value::Null);
                    }
                });
        }
        DataType::Timestamp(TimeUnit::Second, _) => {
            set_temporal_column_as_millis_by_array_type!(
                TimestampSecondArray,
                col_name,
                rows,
                array,
                row_count,
                value_as_datetime
            );
        }
        DataType::Timestamp(TimeUnit::Millisecond, _) => {
            set_temporal_column_as_millis_by_array_type!(
                TimestampMillisecondArray,
                col_name,
                rows,
                array,
                row_count,
                value_as_datetime
            );
        }
        DataType::Timestamp(TimeUnit::Microsecond, _) => {
            set_temporal_column_as_millis_by_array_type!(
                TimestampMicrosecondArray,
                col_name,
                rows,
                array,
                row_count,
                value_as_datetime
            );
        }
        DataType::Timestamp(TimeUnit::Nanosecond, _) => {
            set_temporal_column_as_millis_by_array_type!(
                TimestampNanosecondArray,
                col_name,
                rows,
                array,
                row_count,
                value_as_datetime
            );
        }
        DataType::Time32(TimeUnit::Second) => {
            set_temporal_column_by_array_type!(
                Time32SecondArray,
                col_name,
                rows,
                array,
                row_count,
                value_as_time
            );
        }
        DataType::Time32(TimeUnit::Millisecond) => {
            set_temporal_column_by_array_type!(
                Time32MillisecondArray,
                col_name,
                rows,
                array,
                row_count,
                value_as_time
            );
        }
        DataType::Time64(TimeUnit::Microsecond) => {
            set_temporal_column_by_array_type!(
                Time64MicrosecondArray,
                col_name,
                rows,
                array,
                row_count,
                value_as_time
            );
        }
        DataType::Time64(TimeUnit::Nanosecond) => {
            set_temporal_column_by_array_type!(
                Time64NanosecondArray,
                col_name,
                rows,
                array,
                row_count,
                value_as_time
            );
        }
        DataType::Duration(TimeUnit::Second) => {
            set_temporal_column_by_array_type!(
                DurationSecondArray,
                col_name,
                rows,
                array,
                row_count,
                value_as_duration
            );
        }
        DataType::Duration(TimeUnit::Millisecond) => {
            set_temporal_column_by_array_type!(
                DurationMillisecondArray,
                col_name,
                rows,
                array,
                row_count,
                value_as_duration
            );
        }
        DataType::Duration(TimeUnit::Microsecond) => {
            set_temporal_column_by_array_type!(
                DurationMicrosecondArray,
                col_name,
                rows,
                array,
                row_count,
                value_as_duration
            );
        }
        DataType::Duration(TimeUnit::Nanosecond) => {
            set_temporal_column_by_array_type!(
                DurationNanosecondArray,
                col_name,
                rows,
                array,
                row_count,
                value_as_duration
            );
        }
        DataType::Struct(_) => {
            let inner_objs = struct_array_to_jsonmap_array(as_struct_array(array), row_count)?;
            rows.iter_mut()
                .take(row_count)
                .zip(inner_objs.into_iter())
                .for_each(|(row, obj)| {
                    row.insert(col_name.to_string(), Value::Object(obj));
                });
        }
        DataType::List(_) => {
            let listarr = as_list_array(array);
            rows.iter_mut()
                .zip(listarr.iter())
                .take(row_count)
                .try_for_each(|(row, maybe_value)| -> Result<()> {
                    if let Some(v) = maybe_value {
                        row.insert(col_name.to_string(), Value::Array(array_to_json_array(&v)?));
                    } else {
                        row.insert(col_name.to_string(), Value::Null);
                    }
                    Ok(())
                })?;
        }
        DataType::LargeList(_) => {
            let listarr = as_large_list_array(array);
            rows.iter_mut()
                .zip(listarr.iter())
                .take(row_count)
                .try_for_each(|(row, maybe_value)| -> Result<()> {
                    if let Some(v) = maybe_value {
                        let val = array_to_json_array(&v)?;
                        row.insert(col_name.to_string(), Value::Array(val));
                    }
                    Ok(())
                })?;
        }
        DataType::Dictionary(_, value_type) => {
            let slice = array.slice(0, row_count);
            let hydrated = arrow::compute::kernels::cast::cast(&slice, value_type)
                .expect("cannot cast dictionary to underlying values");
            set_column_for_json_rows(rows, row_count, &hydrated, col_name)?;
        }
        DataType::Decimal128(_, _) => {
            let f64_array = arrow::compute::kernels::cast::cast(array, &DataType::Float64)?;
            set_column_by_primitive_type::<Float64Type>(rows, row_count, &f64_array, col_name);
        }
        DataType::Decimal256(_, _) => {
            let f64_array = arrow::compute::kernels::cast::cast(array, &DataType::Float64)?;
            set_column_by_primitive_type::<Float64Type>(rows, row_count, &f64_array, col_name);
        }
        _ => {
            panic!(
                "Unsupported datatype for JSON serialization: {:#?}",
                array.data_type()
            );
        }
    }
    Ok(())
}

/// Converts an arrow [`RecordBatch`] into a `Vec` of Serde JSON
/// [`JsonMap`]s (objects)
pub fn record_batches_to_json_rows(batches: &[RecordBatch]) -> Result<Vec<JsonMap<String, Value>>> {
    let mut rows: Vec<JsonMap<String, Value>> = iter::repeat(JsonMap::new())
        .take(batches.iter().map(|b| b.num_rows()).sum())
        .collect();

    if !rows.is_empty() {
        let schema = batches[0].schema();
        let mut base = 0;
        for batch in batches {
            let row_count = batch.num_rows();
            for (j, col) in batch.columns().iter().enumerate() {
                let col_name = schema.field(j).name();
                set_column_for_json_rows(&mut rows[base..], row_count, col, col_name)?
            }
            base += row_count;
        }
    }

    Ok(rows)
}

/// This trait defines how to format a sequence of JSON objects to a
/// byte stream.
pub trait JsonFormat: Debug + Default {
    #[inline]
    /// write any bytes needed at the start of the file to the writer
    fn start_stream<W: Write>(&self, _writer: &mut W) -> Result<()> {
        Ok(())
    }

    #[inline]
    /// write any bytes needed for the start of each row
    fn start_row<W: Write>(&self, _writer: &mut W, _is_first_row: bool) -> Result<()> {
        Ok(())
    }

    #[inline]
    /// write any bytes needed for the end of each row
    fn end_row<W: Write>(&self, _writer: &mut W) -> Result<()> {
        Ok(())
    }

    /// write any bytes needed for the start of each row
    fn end_stream<W: Write>(&self, _writer: &mut W) -> Result<()> {
        Ok(())
    }
}

/// Produces JSON output with one record per line. For example
///
/// ```json
/// {"foo":1}
/// {"bar":1}
///
/// ```
#[derive(Debug, Default)]
pub struct LineDelimited {}

impl JsonFormat for LineDelimited {
    fn end_row<W: Write>(&self, writer: &mut W) -> Result<()> {
        writer.write_all(b"\n")?;
        Ok(())
    }
}

/// Produces JSON output as a single JSON array. For example
///
/// ```json
/// [{"foo":1},{"bar":1}]
/// ```
#[derive(Debug, Default)]
pub struct JsonArray {}

impl JsonFormat for JsonArray {
    fn start_stream<W: Write>(&self, writer: &mut W) -> Result<()> {
        writer.write_all(b"[")?;
        Ok(())
    }

    fn start_row<W: Write>(&self, writer: &mut W, is_first_row: bool) -> Result<()> {
        if !is_first_row {
            writer.write_all(b",")?;
        }
        Ok(())
    }

    fn end_stream<W: Write>(&self, writer: &mut W) -> Result<()> {
        writer.write_all(b"]")?;
        Ok(())
    }
}

/// A JSON writer which serializes [`RecordBatch`]es to newline delimited JSON objects
pub type LineDelimitedWriter<W> = Writer<W, LineDelimited>;

/// A JSON writer which serializes [`RecordBatch`]es to JSON arrays
pub type ArrayWriter<W> = Writer<W, JsonArray>;

/// A JSON writer which serializes [`RecordBatch`]es to a stream of
/// `u8` encoded JSON objects. See the module level documentation for
/// detailed usage and examples. The specific format of the stream is
/// controlled by the [`JsonFormat`] type parameter.
#[derive(Debug)]
pub struct Writer<W, F>
where
    W: Write,
    F: JsonFormat,
{
    /// Underlying writer to use to write bytes
    writer: W,

    /// Has the writer output any records yet?
    started: bool,

    /// Is the writer finished?
    finished: bool,

    /// Determines how the byte stream is formatted
    format: F,
}

impl<W, F> Writer<W, F>
where
    W: Write,
    F: JsonFormat,
{
    /// Construct a new writer
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            started: false,
            finished: false,
            format: F::default(),
        }
    }

    /// Write a single JSON row to the output writer
    pub fn write_row(&mut self, row: &Value) -> Result<()> {
        let is_first_row = !self.started;
        if !self.started {
            self.format.start_stream(&mut self.writer)?;
            self.started = true;
        }

        self.format.start_row(&mut self.writer, is_first_row)?;
        self.writer.write_all(
            &serde_json::to_vec(row).map_err(|error| ArrowError::JsonError(error.to_string()))?,
        )?;
        self.format.end_row(&mut self.writer)?;
        Ok(())
    }

    /// Convert the [`RecordBatch`] into JSON rows, and write them to the output
    pub fn write_batches(&mut self, batches: &[RecordBatch]) -> Result<()> {
        for row in record_batches_to_json_rows(batches)? {
            self.write_row(&Value::Object(row))?;
        }
        Ok(())
    }

    /// Finishes the output stream. This function must be called after
    /// all record batches have been produced. (e.g. producing the final `']'` if writing
    /// arrays.
    pub fn finish(&mut self) -> Result<()> {
        if self.started && !self.finished {
            self.format.end_stream(&mut self.writer)?;
            self.finished = true;
        }
        Ok(())
    }

    /// Unwraps this `Writer<W>`, returning the underlying writer
    pub fn into_inner(self) -> W {
        self.writer
    }
}

#[cfg(test)]
mod tests {
    use arrow::buffer::*;
    use arrow::json::reader::*;
    use serde_json::json;
    use std::convert::TryFrom;

    use std::sync::Arc;

    use super::*;

    #[test]
    fn write_simple_rows() {
        let schema = Schema::new(vec![
            Field::new("c1", DataType::Int32, true),
            Field::new("c2", DataType::Utf8, true),
        ]);

        let a = Int32Array::from(vec![Some(1), Some(2), Some(3), None, Some(5)]);
        let b = StringArray::from(vec![Some("a"), Some("b"), Some("c"), Some("d"), None]);

        let batch = RecordBatch::try_new(Arc::new(schema), vec![Arc::new(a), Arc::new(b)]).unwrap();

        let mut buf = Vec::new();
        {
            let mut writer = LineDelimitedWriter::new(&mut buf);
            writer.write_batches(&[batch]).unwrap();
        }

        assert_eq!(
            String::from_utf8(buf).unwrap(),
            r#"{"c1":1,"c2":"a"}
{"c1":2,"c2":"b"}
{"c1":3,"c2":"c"}
{"c1":null,"c2":"d"}
{"c1":5,"c2":null}
"#
        );
    }

    #[test]
    fn write_dictionary() {
        let schema = Schema::new(vec![
            Field::new(
                "c1",
                DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
                true,
            ),
            Field::new(
                "c2",
                DataType::Dictionary(Box::new(DataType::Int8), Box::new(DataType::Utf8)),
                true,
            ),
        ]);

        let a: DictionaryArray<Int32Type> = vec![
            Some("cupcakes"),
            Some("foo"),
            Some("foo"),
            None,
            Some("cupcakes"),
        ]
        .into_iter()
        .collect();
        let b: DictionaryArray<Int8Type> =
            vec![Some("sdsd"), Some("sdsd"), None, Some("sd"), Some("sdsd")]
                .into_iter()
                .collect();

        let batch = RecordBatch::try_new(Arc::new(schema), vec![Arc::new(a), Arc::new(b)]).unwrap();

        let mut buf = Vec::new();
        {
            let mut writer = LineDelimitedWriter::new(&mut buf);
            writer.write_batches(&[batch]).unwrap();
        }

        assert_eq!(
            String::from_utf8(buf).unwrap(),
            r#"{"c1":"cupcakes","c2":"sdsd"}
{"c1":"foo","c2":"sdsd"}
{"c1":"foo","c2":null}
{"c1":null,"c2":"sd"}
{"c1":"cupcakes","c2":"sdsd"}
"#
        );
    }

    #[test]
    fn write_timestamps() {
        let ts_string = "2018-11-13T17:11:10.011375885995";
        let ts_nanos = ts_string
            .parse::<chrono::NaiveDateTime>()
            .unwrap()
            .timestamp_nanos();
        let ts_micros = ts_nanos / 1000;
        let ts_millis = ts_micros / 1000;
        let ts_secs = ts_millis / 1000;

        let arr_nanos = TimestampNanosecondArray::from(vec![Some(ts_nanos), None]);
        let arr_micros = TimestampMicrosecondArray::from(vec![Some(ts_micros), None]);
        let arr_millis = TimestampMillisecondArray::from(vec![Some(ts_millis), None]);
        let arr_secs = TimestampSecondArray::from(vec![Some(ts_secs), None]);
        let arr_names = StringArray::from(vec![Some("a"), Some("b")]);

        let schema = Schema::new(vec![
            Field::new("nanos", arr_nanos.data_type().clone(), true),
            Field::new("micros", arr_micros.data_type().clone(), true),
            Field::new("millis", arr_millis.data_type().clone(), true),
            Field::new("secs", arr_secs.data_type().clone(), true),
            Field::new("name", arr_names.data_type().clone(), true),
        ]);
        let schema = Arc::new(schema);

        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(arr_nanos),
                Arc::new(arr_micros),
                Arc::new(arr_millis),
                Arc::new(arr_secs),
                Arc::new(arr_names),
            ],
        )
        .unwrap();

        let mut buf = Vec::new();
        {
            let mut writer = LineDelimitedWriter::new(&mut buf);
            writer.write_batches(&[batch]).unwrap();
        }

        assert_eq!(
            String::from_utf8(buf).unwrap(),
            r#"{"nanos":1542129070011,"micros":1542129070011,"millis":1542129070011,"secs":1542129070000,"name":"a"}
{"nanos":null,"micros":null,"millis":null,"secs":null,"name":"b"}
"#
        );
    }

    #[test]
    fn write_dates() {
        let ts_string = "2018-11-13T17:11:10.011375885995";
        let ts_millis = ts_string
            .parse::<chrono::NaiveDateTime>()
            .unwrap()
            .timestamp_millis();

        let arr_date32 = Date32Array::from(vec![
            Some(i32::try_from(ts_millis / 1000 / (60 * 60 * 24)).unwrap()),
            None,
        ]);
        let arr_date64 = Date64Array::from(vec![Some(ts_millis), None]);
        let arr_names = StringArray::from(vec![Some("a"), Some("b")]);

        let schema = Schema::new(vec![
            Field::new("date32", arr_date32.data_type().clone(), true),
            Field::new("date64", arr_date64.data_type().clone(), true),
            Field::new("name", arr_names.data_type().clone(), true),
        ]);
        let schema = Arc::new(schema);

        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(arr_date32),
                Arc::new(arr_date64),
                Arc::new(arr_names),
            ],
        )
        .unwrap();

        let mut buf = Vec::new();
        {
            let mut writer = LineDelimitedWriter::new(&mut buf);
            writer.write_batches(&[batch]).unwrap();
        }

        assert_eq!(
            String::from_utf8(buf).unwrap(),
            r#"{"date32":1542067200000,"date64":1542129070011,"name":"a"}
{"date32":null,"date64":null,"name":"b"}
"#
        );
    }

    #[test]
    fn write_times() {
        let arr_time32sec = Time32SecondArray::from(vec![Some(120), None]);
        let arr_time32msec = Time32MillisecondArray::from(vec![Some(120), None]);
        let arr_time64usec = Time64MicrosecondArray::from(vec![Some(120), None]);
        let arr_time64nsec = Time64NanosecondArray::from(vec![Some(120), None]);
        let arr_names = StringArray::from(vec![Some("a"), Some("b")]);

        let schema = Schema::new(vec![
            Field::new("time32sec", arr_time32sec.data_type().clone(), true),
            Field::new("time32msec", arr_time32msec.data_type().clone(), true),
            Field::new("time64usec", arr_time64usec.data_type().clone(), true),
            Field::new("time64nsec", arr_time64nsec.data_type().clone(), true),
            Field::new("name", arr_names.data_type().clone(), true),
        ]);
        let schema = Arc::new(schema);

        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(arr_time32sec),
                Arc::new(arr_time32msec),
                Arc::new(arr_time64usec),
                Arc::new(arr_time64nsec),
                Arc::new(arr_names),
            ],
        )
        .unwrap();

        let mut buf = Vec::new();
        {
            let mut writer = LineDelimitedWriter::new(&mut buf);
            writer.write_batches(&[batch]).unwrap();
        }

        assert_eq!(
            String::from_utf8(buf).unwrap(),
            r#"{"time32sec":"00:02:00","time32msec":"00:00:00.120","time64usec":"00:00:00.000120","time64nsec":"00:00:00.000000120","name":"a"}
{"time32sec":null,"time32msec":null,"time64usec":null,"time64nsec":null,"name":"b"}
"#
        );
    }

    #[test]
    fn write_durations() {
        let arr_durationsec = DurationSecondArray::from(vec![Some(120), None]);
        let arr_durationmsec = DurationMillisecondArray::from(vec![Some(120), None]);
        let arr_durationusec = DurationMicrosecondArray::from(vec![Some(120), None]);
        let arr_durationnsec = DurationNanosecondArray::from(vec![Some(120), None]);
        let arr_names = StringArray::from(vec![Some("a"), Some("b")]);

        let schema = Schema::new(vec![
            Field::new("duration_sec", arr_durationsec.data_type().clone(), true),
            Field::new("duration_msec", arr_durationmsec.data_type().clone(), true),
            Field::new("duration_usec", arr_durationusec.data_type().clone(), true),
            Field::new("duration_nsec", arr_durationnsec.data_type().clone(), true),
            Field::new("name", arr_names.data_type().clone(), true),
        ]);
        let schema = Arc::new(schema);

        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(arr_durationsec),
                Arc::new(arr_durationmsec),
                Arc::new(arr_durationusec),
                Arc::new(arr_durationnsec),
                Arc::new(arr_names),
            ],
        )
        .unwrap();

        let mut buf = Vec::new();
        {
            let mut writer = LineDelimitedWriter::new(&mut buf);
            writer.write_batches(&[batch]).unwrap();
        }

        assert_eq!(
            String::from_utf8(buf).unwrap(),
            r#"{"duration_sec":"PT120S","duration_msec":"PT0.120S","duration_usec":"PT0.000120S","duration_nsec":"PT0.000000120S","name":"a"}
{"duration_sec":null,"duration_msec":null,"duration_usec":null,"duration_nsec":null,"name":"b"}
"#
        );
    }

    #[test]
    fn write_nested_structs() {
        let schema = Schema::new(vec![
            Field::new(
                "c1",
                DataType::Struct(vec![
                    Field::new("c11", DataType::Int32, true),
                    Field::new(
                        "c12",
                        DataType::Struct(vec![Field::new("c121", DataType::Utf8, false)]),
                        false,
                    ),
                ]),
                false,
            ),
            Field::new("c2", DataType::Utf8, false),
        ]);

        let c1 = StructArray::from(vec![
            (
                Field::new("c11", DataType::Int32, true),
                Arc::new(Int32Array::from(vec![Some(1), None, Some(5)])) as ArrayRef,
            ),
            (
                Field::new(
                    "c12",
                    DataType::Struct(vec![Field::new("c121", DataType::Utf8, false)]),
                    false,
                ),
                Arc::new(StructArray::from(vec![(
                    Field::new("c121", DataType::Utf8, false),
                    Arc::new(StringArray::from(vec![Some("e"), Some("f"), Some("g")])) as ArrayRef,
                )])) as ArrayRef,
            ),
        ]);
        let c2 = StringArray::from(vec![Some("a"), Some("b"), Some("c")]);

        let batch =
            RecordBatch::try_new(Arc::new(schema), vec![Arc::new(c1), Arc::new(c2)]).unwrap();

        let mut buf = Vec::new();
        {
            let mut writer = LineDelimitedWriter::new(&mut buf);
            writer.write_batches(&[batch]).unwrap();
        }

        assert_eq!(
            String::from_utf8(buf).unwrap(),
            r#"{"c1":{"c11":1,"c12":{"c121":"e"}},"c2":"a"}
{"c1":{"c11":null,"c12":{"c121":"f"}},"c2":"b"}
{"c1":{"c11":5,"c12":{"c121":"g"}},"c2":"c"}
"#
        );
    }

    #[test]
    fn write_struct_with_list_field() {
        let field_c1 = Field::new(
            "c1",
            DataType::List(Box::new(Field::new("c_list", DataType::Utf8, false))),
            false,
        );
        let field_c2 = Field::new("c2", DataType::Int32, false);
        let schema = Schema::new(vec![field_c1.clone(), field_c2]);

        let a_values = StringArray::from(vec!["a", "a1", "b", "c", "d", "e"]);
        // list column rows: ["a", "a1"], ["b"], ["c"], ["d"], ["e"]
        let a_value_offsets = Buffer::from(&[0, 2, 3, 4, 5, 6].to_byte_slice());
        let a_list_data = ArrayData::builder(field_c1.data_type().clone())
            .len(5)
            .add_buffer(a_value_offsets)
            .add_child_data(a_values.data().clone())
            .null_bit_buffer(Some(Buffer::from(vec![0b00011111])))
            .build()
            .unwrap();
        let a = ListArray::from(a_list_data);

        let b = Int32Array::from(vec![1, 2, 3, 4, 5]);

        let batch = RecordBatch::try_new(Arc::new(schema), vec![Arc::new(a), Arc::new(b)]).unwrap();

        let mut buf = Vec::new();
        {
            let mut writer = LineDelimitedWriter::new(&mut buf);
            writer.write_batches(&[batch]).unwrap();
        }

        assert_eq!(
            String::from_utf8(buf).unwrap(),
            r#"{"c1":["a","a1"],"c2":1}
{"c1":["b"],"c2":2}
{"c1":["c"],"c2":3}
{"c1":["d"],"c2":4}
{"c1":["e"],"c2":5}
"#
        );
    }

    #[test]
    fn write_nested_list() {
        let list_inner_type = Field::new(
            "a",
            DataType::List(Box::new(Field::new("b", DataType::Int32, true))),
            false,
        );
        let field_c1 = Field::new(
            "c1",
            DataType::List(Box::new(list_inner_type.clone())),
            false,
        );
        let field_c2 = Field::new("c2", DataType::Utf8, true);
        let schema = Schema::new(vec![field_c1.clone(), field_c2]);

        // list column rows: [[1, 2], [3]], [], [[4, 5, 6]]
        let a_values = Int32Array::from(vec![1, 2, 3, 4, 5, 6]);

        let a_value_offsets = Buffer::from(&[0, 2, 3, 6].to_byte_slice());
        // Construct a list array from the above two
        let a_list_data = ArrayData::builder(list_inner_type.data_type().clone())
            .len(3)
            .add_buffer(a_value_offsets)
            .null_bit_buffer(Some(Buffer::from(vec![0b00000111])))
            .add_child_data(a_values.data().clone())
            .build()
            .unwrap();

        let c1_value_offsets = Buffer::from(&[0, 2, 2, 3].to_byte_slice());
        let c1_list_data = ArrayData::builder(field_c1.data_type().clone())
            .len(3)
            .add_buffer(c1_value_offsets)
            .add_child_data(a_list_data)
            .build()
            .unwrap();

        let c1 = ListArray::from(c1_list_data);
        let c2 = StringArray::from(vec![Some("foo"), Some("bar"), None]);

        let batch =
            RecordBatch::try_new(Arc::new(schema), vec![Arc::new(c1), Arc::new(c2)]).unwrap();

        let mut buf = Vec::new();
        {
            let mut writer = LineDelimitedWriter::new(&mut buf);
            writer.write_batches(&[batch]).unwrap();
        }

        assert_eq!(
            String::from_utf8(buf).unwrap(),
            r#"{"c1":[[1,2],[3]],"c2":"foo"}
{"c1":[],"c2":"bar"}
{"c1":[[4,5,6]],"c2":null}
"#
        );
    }

    #[test]
    fn write_list_of_struct() {
        let field_c1 = Field::new(
            "c1",
            DataType::List(Box::new(Field::new(
                "s",
                DataType::Struct(vec![
                    Field::new("c11", DataType::Int32, true),
                    Field::new(
                        "c12",
                        DataType::Struct(vec![Field::new("c121", DataType::Utf8, false)]),
                        false,
                    ),
                ]),
                false,
            ))),
            true,
        );
        let field_c2 = Field::new("c2", DataType::Int32, false);
        let schema = Schema::new(vec![field_c1.clone(), field_c2]);

        let struct_values = StructArray::from(vec![
            (
                Field::new("c11", DataType::Int32, true),
                Arc::new(Int32Array::from(vec![Some(1), None, Some(5)])) as ArrayRef,
            ),
            (
                Field::new(
                    "c12",
                    DataType::Struct(vec![Field::new("c121", DataType::Utf8, false)]),
                    false,
                ),
                Arc::new(StructArray::from(vec![(
                    Field::new("c121", DataType::Utf8, false),
                    Arc::new(StringArray::from(vec![Some("e"), Some("f"), Some("g")])) as ArrayRef,
                )])) as ArrayRef,
            ),
        ]);

        // list column rows (c1):
        // [{"c11": 1, "c12": {"c121": "e"}}, {"c12": {"c121": "f"}}],
        // null,
        // [{"c11": 5, "c12": {"c121": "g"}}]
        let c1_value_offsets = Buffer::from(&[0, 2, 2, 3].to_byte_slice());
        let c1_list_data = ArrayData::builder(field_c1.data_type().clone())
            .len(3)
            .add_buffer(c1_value_offsets)
            .add_child_data(struct_values.data().clone())
            .null_bit_buffer(Some(Buffer::from(vec![0b00000101])))
            .build()
            .unwrap();
        let c1 = ListArray::from(c1_list_data);

        let c2 = Int32Array::from(vec![1, 2, 3]);

        let batch =
            RecordBatch::try_new(Arc::new(schema), vec![Arc::new(c1), Arc::new(c2)]).unwrap();

        let mut buf = Vec::new();
        {
            let mut writer = LineDelimitedWriter::new(&mut buf);
            writer.write_batches(&[batch]).unwrap();
        }

        assert_eq!(
            String::from_utf8(buf).unwrap(),
            r#"{"c1":[{"c11":1,"c12":{"c121":"e"}},{"c11":null,"c12":{"c121":"f"}}],"c2":1}
{"c1":null,"c2":2}
{"c1":[{"c11":5,"c12":{"c121":"g"}}],"c2":3}
"#
        );
    }

    #[test]
    fn json_writer_empty() {
        let mut writer = ArrayWriter::new(vec![] as Vec<u8>);
        writer.finish().unwrap();
        assert_eq!(String::from_utf8(writer.into_inner()).unwrap(), "");
    }
    #[test]
    fn json_writer_one_row() {
        let mut writer = ArrayWriter::new(vec![] as Vec<u8>);
        let v = json!({ "an": "object" });
        writer.write_row(&v).unwrap();
        writer.finish().unwrap();
        assert_eq!(
            String::from_utf8(writer.into_inner()).unwrap(),
            r#"[{"an":"object"}]"#
        );
    }

    #[test]
    fn json_writer_two_rows() {
        let mut writer = ArrayWriter::new(vec![] as Vec<u8>);
        let v = json!({ "an": "object" });
        writer.write_row(&v).unwrap();
        let v = json!({ "another": "object" });
        writer.write_row(&v).unwrap();
        writer.finish().unwrap();
        assert_eq!(
            String::from_utf8(writer.into_inner()).unwrap(),
            r#"[{"an":"object"},{"another":"object"}]"#
        );
    }

    #[test]
    fn json_list_roundtrip() {
        let json_content = r#"
        {"list": [{"ints": 1}]}
        {"list": [{}]}
        {"list": []}
        {"list": null}
        {"list": [{"ints": null}]}
        {"list": [null]}
        "#;
        let ints_struct = DataType::Struct(vec![Field::new("ints", DataType::Int32, true)]);
        let list_type = DataType::List(Box::new(Field::new("item", ints_struct, true)));
        let list_field = Field::new("list", list_type, true);
        let schema = Arc::new(Schema::new(vec![list_field]));
        let builder = ReaderBuilder::new().with_schema(schema).with_batch_size(64);
        let mut reader = builder.build(std::io::Cursor::new(json_content)).unwrap();

        let batch = reader.next().unwrap().unwrap();

        let list_row = batch
            .column(0)
            .as_any()
            .downcast_ref::<ListArray>()
            .unwrap();
        let values = list_row.values();
        assert_eq!(values.len(), 4);
        assert_eq!(values.null_count(), 1);

        // write the batch to JSON, and compare output with input
        let mut buf = Vec::new();
        {
            let mut writer = LineDelimitedWriter::new(&mut buf);
            writer.write_batches(&[batch]).unwrap();
        }

        // NOTE: The last value should technically be {"list": [null]} but it appears
        // that implementations differ on the treatment of a null struct.
        // It would be more accurate to return a null struct, so this can be done
        // as a follow up.
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            r#"{"list":[{"ints":1}]}
{"list":[{"ints":null}]}
{"list":[]}
{"list":null}
{"list":[{"ints":null}]}
{"list":[{"ints":null}]}
"#
        );
    }
}
