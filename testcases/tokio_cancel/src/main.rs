// Gap: task cancellation. A future holding validated data reaches the sink
// only in Drop. Aborting the task makes the runtime destroy it through its
// task vtable, without returning through the validating submit function.
// want: OK
// today: FAIL
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::{runtime::Builder, task::JoinHandle};

struct Job(u64);

impl Future for Job {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        Poll::Pending
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        execute(self.0);
    }
}

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

#[tinted::validate(data)]
fn submit(x: u64) -> JoinHandle<()> {
    tokio::spawn(Job(x))
}

fn main() {
    Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let task = submit(7);
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        });
}
