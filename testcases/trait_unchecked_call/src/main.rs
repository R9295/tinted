// Negative: trait objects. The implementation that reaches the sink is also
// called through a trait object without validation.
// want: FAIL
// today: FAIL
use std::hint::black_box;

trait Job {
    fn run(&self, x: u64);
}

struct Dangerous;

impl Job for Dangerous {
    fn run(&self, x: u64) {
        execute(x);
    }
}

#[tinted::sink(data)]
fn execute(x: u64) {
    black_box(x);
}

#[tinted::validate(data)]
fn checked(job: &dyn Job) {
    job.run(7);
}

fn unchecked(job: &dyn Job) {
    job.run(7);
}

fn main() {
    checked(black_box(&Dangerous));
    unchecked(black_box(&Dangerous));
}
