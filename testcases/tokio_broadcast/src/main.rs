// Coverage: tokio broadcast channels. Data is validated before it is sent
// and executed by a subscriber.
// want: OK
// today: OK
use tokio::runtime::Builder;
use tokio::sync::broadcast::{Receiver, Sender, channel};

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

async fn consume(mut rx: Receiver<u64>) {
    while let Ok(x) = rx.recv().await {
        execute(x);
    }
}

fn main() {
    let runtime = Builder::new_multi_thread().build().unwrap();
    runtime.block_on(async {
        let (tx, rx) = channel(4);
        let worker = tokio::spawn(consume(rx));
        produce(tx);
        worker.await.unwrap();
    });
}
