// Gap: two methods of one trait with the same signature share a type id.
// `peek` is called without validation and `run` reaches the sink, and the two
// cannot be told apart.
// want: OK
// today: FAIL
use std::hint::black_box;

trait Job {
    fn peek(&self, x: u64);
    fn run(&self, x: u64);
}

struct Printer;

impl Job for Printer {
    fn peek(&self, x: u64) {
        println!("peek {x}");
    }
    fn run(&self, x: u64) {
        execute(x);
    }
}

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

#[tinted::validate(data)]
fn run_checked(job: &dyn Job) {
    job.run(1);
}

fn inspect(job: &dyn Job) {
    job.peek(2);
}

fn main() {
    let job: &dyn Job = black_box(&Printer);
    run_checked(job);
    inspect(job);
}
