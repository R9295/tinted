// Gap: indirect calls are matched by type. `run_unchecked` is only ever given
// a harmless function, but that has the same type as the function that
// reaches the sink, so it is taken for a caller of it.
// want: OK
// today: FAIL
use std::hint::black_box;

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

fn harmless(x: u64) {
    println!("harmless {x}");
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
    run_unchecked(black_box(harmless));
}
