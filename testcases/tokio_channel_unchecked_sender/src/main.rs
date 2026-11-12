// Negative: tokio channels. One sender validates before it sends, but a
// second sender puts unvalidated data on the same channel.
// want: FAIL
// today: FAIL
use tokio::runtime::Builder;
use tokio::sync::mpsc::{Receiver, Sender, channel};

fn check(x: u64) -> u64 {
    x
}

#[tinted::validate(data)]
async fn produce(tx: Sender<u64>) {
    tx.send(check(7)).await.unwrap();
}

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

async fn consume(mut rx: Receiver<u64>) {
    while let Some(x) = rx.recv().await {
        execute(x);
    }
}

fn main() {
    let runtime = Builder::new_multi_thread().build().unwrap();
    runtime.block_on(async {
        let (tx, rx) = channel(4);
        let worker = tokio::spawn(consume(rx));
        let tx2 = tx.clone();
        tx2.send(1000).await.unwrap();
        drop(tx2);
        produce(tx).await;
        worker.await.unwrap();
    });
}
