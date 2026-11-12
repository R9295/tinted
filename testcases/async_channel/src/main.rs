// Coverage: async-channel. Data is validated before it is sent and executed
// by the code that receives it.
// want: OK
// today: OK
use async_channel::{Receiver, Sender, bounded};
use futures::executor::block_on;

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

async fn consume(rx: Receiver<u64>) {
    while let Ok(x) = rx.recv().await {
        execute(x);
    }
}

fn main() {
    block_on(async {
        let (tx, rx) = bounded(4);
        produce(tx).await;
        consume(rx).await;
    });
}
