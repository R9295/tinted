// Gap: different implementations of the same trait method share a type id.
// Only Dangerous reaches the sink, and it is only called under validation,
// but the unchecked call on Harmless is also connected to Dangerous::run.
// want: OK
// today: FAIL
use std::hint::black_box;

trait Job {
    fn run(&self, x: u64);
}

struct Dangerous;
struct Harmless;

impl Job for Dangerous {
    fn run(&self, x: u64) {
        execute(x);
    }
}

impl Job for Harmless {
    fn run(&self, x: u64) {
        black_box(x);
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
    unchecked(black_box(&Harmless));
}
