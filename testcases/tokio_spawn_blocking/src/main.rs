// Coverage: blocking tasks. A closure captures validated data, but a blocking
// pool worker invokes it through the runtime's task machinery.
// want: OK
// today: OK
use tokio::runtime::Builder;

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

#[tinted::validate(data)]
async fn process(x: u64) {
    tokio::task::spawn_blocking(move || execute(x))
        .await
        .unwrap();
}

fn main() {
    Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(process(7));
}
