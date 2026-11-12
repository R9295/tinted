// Negative: futures channels. One sender validates before it sends, but a
// second sender puts unvalidated data on the same channel.
// want: FAIL
// today: FAIL
use futures::channel::mpsc::{Receiver, Sender, channel};
use futures::executor::block_on;
use futures::{SinkExt, StreamExt};

fn check(x: u64) -> u64 {
    x
}

#[tinted::validate(data)]
async fn produce(mut tx: Sender<u64>) {
    tx.send(check(7)).await.unwrap();
}

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

async fn consume(mut rx: Receiver<u64>) {
    while let Some(x) = rx.next().await {
        execute(x);
    }
}

fn main() {
    block_on(async {
        let (tx, rx) = channel(4);
        let mut tx2 = tx.clone();
        tx2.send(1000).await.unwrap();
        drop(tx2);
        produce(tx).await;
        consume(rx).await;
    });
}
