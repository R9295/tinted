// Negative: tokio tasks. One task is spawned under validation, and a second
// task that reaches the sink is spawned outside it.
// want: FAIL
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
    runtime.block_on(async { Handle::current().spawn(async move { execute(1000) }).await.unwrap() });
}
