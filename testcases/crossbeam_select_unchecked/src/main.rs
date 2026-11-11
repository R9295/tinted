// Negative: crossbeam `select!`. One of the two channels the receiving
// thread waits on carries data that was never validated.
// want: FAIL
// today: FAIL
use crossbeam_channel::{Receiver, Sender, select, unbounded};
use std::thread;

fn check(x: u64) -> u64 {
    x
}

#[tinted::validate(data)]
fn produce(tx: Sender<u64>, x: u64) {
    tx.send(check(x)).unwrap();
}

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

fn consume(a: Receiver<u64>, b: Receiver<u64>) {
    for _ in 0..2 {
        select! {
            recv(a) -> x => execute(x.unwrap()),
            recv(b) -> x => execute(x.unwrap()),
        }
    }
}

fn main() {
    let (tx_a, rx_a) = unbounded();
    let (tx_b, rx_b) = unbounded();
    let worker = thread::spawn(move || consume(rx_a, rx_b));
    produce(tx_a, 7);
    tx_b.send(1000).unwrap();
    worker.join().unwrap();
}
