// Coverage: futures oneshot channels. The receiver is itself the future that
// yields the validated value.
// want: OK
// today: OK
use futures::channel::oneshot::{Receiver, Sender, channel};
use futures::executor::block_on;

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
    block_on(async {
        let (tx, rx) = channel();
        produce(tx);
        consume(rx).await;
    });
}
