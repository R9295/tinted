// Coverage: crossbeam `select!`. The receiving thread waits on two channels
// at once; both carry data validated before it was sent.
// want: OK
// today: OK
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
    produce(tx_b, 8);
    worker.join().unwrap();
}
