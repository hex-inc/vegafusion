/*
 * VegaFusion
 * Copyright (C) 2022 VegaFusion Technologies LLC
 *
 * This program is distributed under multiple licenses.
 * Please consult the license documentation provided alongside
 * this program the details of the active license.
 */

use tonic::{transport::Server, Request, Response, Status};
use vegafusion_core::error::{ResultWithContext, VegaFusionError};
use vegafusion_core::proto::gen::services::vega_fusion_runtime_server::{
    VegaFusionRuntime as TonicVegaFusionRuntime,
    VegaFusionRuntimeServer as TonicVegaFusionRuntimeServer,
};
use vegafusion_core::proto::gen::services::{PreTransformSpecResult, PreTransformValuesResult, QueryRequest, QueryResult};
use vegafusion_rt_datafusion::task_graph::runtime::TaskGraphRuntime;

use clap::Parser;
use regex::Regex;
use vegafusion_core::proto::gen::pretransform::{PreTransformSpecRequest, PreTransformValuesRequest};

#[derive(Clone)]
pub struct VegaFusionRuntimeGrpc {
    pub runtime: TaskGraphRuntime,
}

impl VegaFusionRuntimeGrpc {
    pub fn new(runtime: TaskGraphRuntime) -> VegaFusionRuntimeGrpc {
        VegaFusionRuntimeGrpc { runtime }
    }
}

#[tonic::async_trait]
impl TonicVegaFusionRuntime for VegaFusionRuntimeGrpc {
    async fn task_graph_query(
        &self,
        request: Request<QueryRequest>,
    ) -> Result<Response<QueryResult>, Status> {
        println!("grpc request...");
        let result = self.runtime.query_request(request.into_inner()).await;
        match result {
            Ok(result) => {
                println!("  response");
                Ok(Response::new(result))
            }
            Err(err) => Err(Status::unknown(err.to_string())),
        }
    }

    async fn pre_transform_spec(
        &self,
        request: Request<PreTransformSpecRequest>,
    ) -> Result<Response<PreTransformSpecResult>, Status> {
        let result = self
            .runtime
            .pre_transform_spec_request(request.into_inner())
            .await;
        match result {
            Ok(result) => Ok(Response::new(result)),
            Err(err) => Err(Status::unknown(err.to_string())),
        }
    }

    async fn pre_transform_values(&self, request: Request<PreTransformValuesRequest>) -> Result<Response<PreTransformValuesResult>, Status> {
        let result = self
            .runtime
            .pre_transform_values_request(request.into_inner())
            .await;
        match result {
            Ok(result) => Ok(Response::new(result)),
            Err(err) => Err(Status::unknown(err.to_string())),
        }
    }
}

/// VegaFusion Server
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Hostname
    #[clap(long, default_value = "127.0.0.1")]
    pub host: String,

    /// Port for gRPC server
    #[clap(long, default_value = "50051")]
    pub port: u32,

    /// Cache capacity
    #[clap(long, default_value = "64")]
    pub capacity: usize,

    /// Cache memory limit
    #[clap(long)]
    pub memory_limit: Option<String>,

    /// Include compatibility with gRPC-Web
    #[clap(long, takes_value = false)]
    pub web: bool,
}

#[tokio::main]
async fn main() -> Result<(), VegaFusionError> {
    let args = Args::parse();

    // Create addresse
    let grpc_address = format!("{}:{}", args.host, args.port);

    // Log Capacity limit
    println!("Cache capacity limit: {} entries", args.capacity);

    // Handle memory
    let memory_limit = if let Some(memory_limit) = &args.memory_limit {
        let memory_limit = parse_memory_string(memory_limit)?;
        println!("Cache memory limit: {} bytes", memory_limit);
        Some(memory_limit)
    } else {
        println!("No cache memory limit");
        None
    };

    let tg_runtime = TaskGraphRuntime::new(Some(args.capacity), memory_limit);

    grpc_server(grpc_address, tg_runtime.clone(), args.web)
        .await
        .expect("Failed to start grpc service");

    Ok(())
}

fn parse_memory_string(memory_limit: &str) -> Result<usize, VegaFusionError> {
    let pattern = Regex::new(r"(^\d+(\.\d+)?)(g|gb|gib|m|mb|mib|k|kb|kib|b)?$").unwrap();
    match pattern.captures(&memory_limit.to_lowercase()) {
        Some(captures) => {
            let amount: f64 = captures.get(1).unwrap().as_str().parse().unwrap();
            let suffix = captures.get(3).map(|c| c.as_str()).unwrap_or("b");
            let factor = match suffix {
                "b" => 1,
                "k" | "kb" => 1000,
                "kib" => 1024,
                "m" | "mb" => 1_000_000,
                "mib" => 1024 * 1024,
                "g" | "gb" => 1_000_000_000,
                "gib" => 1024 * 1024 * 1024,
                _ => panic!("Unreachable"),
            };
            let total = (amount * factor as f64) as usize;
            Ok(total)
        }
        None => Err(VegaFusionError::parse(format!(
            "Unable to parse memory limit: {}",
            memory_limit
        ))),
    }
}

async fn grpc_server(
    address: String,
    runtime: TaskGraphRuntime,
    web: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let addr = address
        .parse()
        .ok()
        .with_context(|| format!("Failed to parse address: {}", address))?;
    let server = TonicVegaFusionRuntimeServer::new(VegaFusionRuntimeGrpc::new(runtime));

    if web {
        println!("Starting gRPC + gRPC-Web server on {}", address);
        let server = tonic_web::config().enable(server);
        Server::builder()
            .accept_http1(true)
            .add_service(server)
            .serve(addr)
            .await?;
    } else {
        println!("Starting gRPC server on {}", address);
        Server::builder().add_service(server).serve(addr).await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::parse_memory_string;

    #[test]
    fn test_parse_memory_string() {
        fn check(memory_str: &str, expected: usize) {
            assert_eq!(parse_memory_string(memory_str).unwrap(), expected)
        }

        check("123", 123);
        check("123.2", 123);
        check("123b", 123);
        check("123.2b", 123);

        check("123kb", 123e3 as usize);
        check("123.2KB", 123.2e3 as usize);
        check("123Kib", 123 * 1024);
        check("123.2Kib", (123.2 * 1024.0) as usize);

        check("123mb", 123e6 as usize);
        check("123.2MB", 123.2e6 as usize);
        check("123Mib", 123 * 1024 * 1024);
        check("123.2Mib", (123.2 * 1024.0 * 1024.0) as usize);

        check("123gb", 123e9 as usize);
        check("123.2GB", 123.2e9 as usize);
        check("123Gib", 123 * 1024 * 1024 * 1024);
        check("123.2Gib", (123.2 * 1024.0 * 1024.0 * 1024.0) as usize);
    }
}
