// Coverage: local tasks. The LocalSet runs a future holding validated data
// through its task machinery. Staying on one thread does not preserve the
// validating future's call chain.
// want: OK
// today: OK
use std::rc::Rc;
use tokio::{runtime::Builder, task::LocalSet};

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

#[tinted::validate(data)]
async fn process(x: u64) {
    // Rc makes this future's captured data non-Send, requiring a local task.
    let data = Rc::new(x);
    tokio::task::spawn_local(async move { execute(*data) })
        .await
        .unwrap();
}

fn main() {
    let runtime = Builder::new_current_thread().build().unwrap();
    LocalSet::new().block_on(&runtime, process(7));
}
