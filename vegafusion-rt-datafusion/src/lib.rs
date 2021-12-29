#[macro_use]
extern crate lazy_static;

pub mod data;
pub mod expression;
pub mod task_graph;
pub mod tokio_runtime;
pub mod transform;
pub mod signal;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
