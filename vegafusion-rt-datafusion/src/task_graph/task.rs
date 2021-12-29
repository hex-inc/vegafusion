use async_trait::async_trait;
use std::convert::TryInto;
use vegafusion_core::error::Result;
use vegafusion_core::proto::gen::tasks::task::TaskKind;
use vegafusion_core::proto::gen::tasks::Task;
use vegafusion_core::task_graph::task_value::TaskValue;

#[async_trait]
pub trait TaskCall {
    async fn eval(&self, values: &[TaskValue]) -> Result<(TaskValue, Vec<TaskValue>)>;
}

#[async_trait]
impl TaskCall for Task {
    async fn eval(&self, values: &[TaskValue]) -> Result<(TaskValue, Vec<TaskValue>)> {
        match self.task_kind() {
            TaskKind::Value(value) => Ok((value.try_into()?, Default::default())),
            TaskKind::DataUrl(task) => task.eval(values).await,
            TaskKind::DataValues(task) => task.eval(values).await,
            TaskKind::DataSource(task) => task.eval(values).await,
            TaskKind::Signal(task) => task.eval(values).await,
        }
    }
}
