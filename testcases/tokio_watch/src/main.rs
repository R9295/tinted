// Coverage: tokio watch channels. The receiver reads the validated value by
// borrowing it from the channel.
// want: OK
// today: OK
use tokio::runtime::Builder;
use tokio::sync::watch::{Receiver, Sender, channel};

fn check(x: u64) -> u64 {
    x
}

#[tinted::validate(data)]
fn produce(tx: &Sender<u64>) {
    tx.send(check(7)).unwrap();
}

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

async fn consume(mut rx: Receiver<u64>) {
    while rx.changed().await.is_ok() {
        let x = *rx.borrow_and_update();
        execute(x);
    }
}

fn main() {
    let runtime = Builder::new_multi_thread().build().unwrap();
    runtime.block_on(async {
        let (tx, rx) = channel(0);
        let worker = tokio::spawn(consume(rx));
        produce(&tx);
        drop(tx);
        worker.await.unwrap();
    });
}
