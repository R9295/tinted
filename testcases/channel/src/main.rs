// Gap: channels. Data is validated before it is sent, but a send is not a
// call, so the chain from the sink ends in the receiving thread.
// want: OK
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
    produce(tx);
    worker.join().unwrap();
}
