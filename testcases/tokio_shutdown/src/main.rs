// Coverage: runtime shutdown. A pending future holds validated data and reaches
// the sink only in Drop. Dropping the runtime destroys the future outside
// the validating submit function, even if the future was never polled.
// want: OK
// today: OK
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
    let runtime = Builder::new_current_thread().build().unwrap();
    let task = {
        let _entered = runtime.enter();
        submit(7)
    };
    drop(runtime);
    // Runtime shutdown has already destroyed the future, even though its
    // join handle is still alive.
    assert!(task.is_finished());
    drop(task);
}
