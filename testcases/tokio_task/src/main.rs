// Gap: tokio tasks. The task is spawned under the validate function, but it
// is run by a tokio worker through the task's vtable, not called by it.
// want: OK
// today: FAIL
use tokio::runtime::{Builder, Handle};

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

#[tinted::validate(data)]
async fn process(x: u64) {
    Handle::current().spawn(async move { execute(x) }).await.unwrap();
}

fn main() {
    let runtime = Builder::new_multi_thread().build().unwrap();
    runtime.block_on(process(7));
}
