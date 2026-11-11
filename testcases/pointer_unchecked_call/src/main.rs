// Negative: function pointers. The function that reaches the sink is also
// handed to a runner that does not validate.
// want: FAIL
// today: FAIL
use std::hint::black_box;

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

#[tinted::validate(data)]
fn run_checked(job: fn(u64)) {
    job(1);
}

fn run_unchecked(job: fn(u64)) {
    job(2);
}

fn main() {
    run_checked(black_box(execute));
    run_unchecked(black_box(execute));
}
