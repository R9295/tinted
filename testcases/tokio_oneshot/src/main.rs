// Coverage: tokio oneshot channels. The receiver is itself the future that
// yields the validated value.
// want: OK
// today: OK
use tokio::runtime::Builder;
use tokio::sync::oneshot::{Receiver, Sender, channel};

fn check(x: u64) -> u64 {
    x
}

#[tinted::validate(data)]
fn produce(tx: Sender<u64>) {
    tx.send(check(7)).unwrap();
}

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

async fn consume(rx: Receiver<u64>) {
    if let Ok(x) = rx.await {
        execute(x);
    }
}

fn main() {
    let runtime = Builder::new_multi_thread().build().unwrap();
    runtime.block_on(async {
        let (tx, rx) = channel();
        let worker = tokio::spawn(consume(rx));
        produce(tx);
        worker.await.unwrap();
    });
}
