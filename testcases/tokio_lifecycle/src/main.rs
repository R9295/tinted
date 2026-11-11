// Coverage: runtime lifecycle callbacks. Validated data is captured when the
// runtime is configured, but its worker invokes the start, stop, park and
// unpark callbacks through stored trait objects.
// want: OK
// today: OK
use tokio::runtime::{Builder, Runtime};

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

#[tinted::validate(data)]
fn configure(x: u64) -> Runtime {
    Builder::new_multi_thread()
        .worker_threads(1)
        .on_thread_start(move || execute(x))
        .on_thread_stop(move || execute(x))
        .on_thread_park(move || execute(x))
        .on_thread_unpark(move || execute(x))
        .build()
        .unwrap()
}

fn main() {
    let runtime = configure(7);
    runtime.block_on(runtime.spawn(async {})).unwrap();
    drop(runtime);
}
