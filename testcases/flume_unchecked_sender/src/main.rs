// Negative: flume channels. A second sender puts unvalidated data on the same
// channel.
// want: FAIL
// today: FAIL
use flume::{Receiver, Sender, unbounded};
use std::thread;

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

fn consume(rx: Receiver<u64>) {
    while let Ok(x) = rx.recv() {
        execute(x);
    }
}

fn main() {
    let (tx, rx) = unbounded();
    let worker = thread::spawn(move || consume(rx));
    let tx2 = tx.clone();
    tx2.send(1000).unwrap();
    drop(tx2);
    produce(tx);
    worker.join().unwrap();
}
