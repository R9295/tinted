// Negative: deferred work. One job is queued under validation, and a second
// job that reaches the sink is queued without it.
// want: FAIL
// today: FAIL
use std::{sync::mpsc, thread};

type Job = Box<dyn FnOnce() + Send>;

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

#[tinted::validate(data)]
fn submit(queue: &mpsc::Sender<Job>, x: u64) {
    queue.send(Box::new(move || execute(x))).unwrap();
}

fn work(queue: mpsc::Receiver<Job>) {
    for job in queue {
        job();
    }
}

fn main() {
    let (tx, rx) = mpsc::channel::<Job>();
    let worker = thread::spawn(move || work(rx));
    submit(&tx, 7);
    tx.send(Box::new(move || execute(1000))).unwrap();
    drop(tx);
    worker.join().unwrap();
}
