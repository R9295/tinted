// Coverage: crossbeam bounded channels and the non-blocking calls. Data is
// validated before `try_send` and executed after `recv_timeout`.
// want: OK
// today: OK
use crossbeam_channel::{Receiver, Sender, bounded};
use std::{thread, time::Duration};

fn check(x: u64) -> u64 {
    x
}

#[tinted::validate(data)]
fn produce(tx: Sender<u64>) {
    tx.try_send(check(7)).unwrap();
}

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

fn consume(rx: Receiver<u64>) {
    while let Ok(x) = rx.recv_timeout(Duration::from_secs(1)) {
        execute(x);
    }
}

fn main() {
    let (tx, rx) = bounded(4);
    let worker = thread::spawn(move || consume(rx));
    produce(tx);
    worker.join().unwrap();
}
