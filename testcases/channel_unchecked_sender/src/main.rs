// Negative: channels. One sender validates before it sends, but a second
// sender puts unvalidated data on the same channel.
// want: FAIL
// today: FAIL
use std::{sync::mpsc, thread};

fn check(x: u64) -> u64 {
    x
}

#[tinted::validate(data)]
fn produce(tx: mpsc::Sender<u64>) {
    tx.send(check(7)).unwrap();
}

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

fn consume(rx: mpsc::Receiver<u64>) {
    for x in rx {
        execute(x);
    }
}

fn main() {
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || consume(rx));
    let tx2 = tx.clone();
    tx2.send(1000).unwrap();
    drop(tx2);
    produce(tx);
    worker.join().unwrap();
}
