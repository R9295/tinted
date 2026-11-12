// Coverage: tokio unbounded channels. The send is synchronous and the
// receiver is a blocking thread, not a task.
// want: OK
// today: OK
use std::thread;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

fn check(x: u64) -> u64 {
    x
}

#[tinted::validate(data)]
fn produce(tx: UnboundedSender<u64>) {
    tx.send(check(7)).unwrap();
}

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

fn consume(mut rx: UnboundedReceiver<u64>) {
    while let Some(x) = rx.blocking_recv() {
        execute(x);
    }
}

fn main() {
    let (tx, rx) = unbounded_channel();
    let worker = thread::spawn(move || consume(rx));
    produce(tx);
    worker.join().unwrap();
}
