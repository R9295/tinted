// Coverage: crossbeam channels. Data is validated before it is sent on a
// crossbeam channel and executed by the receiving thread.
// want: OK
// today: OK
use crossbeam_channel::{Receiver, Sender, unbounded};
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
    for x in rx {
        execute(x);
    }
}

fn main() {
    let (tx, rx) = unbounded();
    let worker = thread::spawn(move || consume(rx));
    produce(tx);
    worker.join().unwrap();
}
