// Gap: formatting callbacks. Display::fmt only runs under validation, but
// the code that invokes its function pointer is in the prebuilt standard
// library. The chain ends at the formatting function-pointer shim.
// want: OK
// today: FAIL
use std::{fmt, hint::black_box};

struct Job(u64);

impl fmt::Display for Job {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        execute(self.0);
        f.write_str("job")
    }
}

#[tinted::sink(data)]
fn execute(x: u64) {
    black_box(x);
}

#[tinted::validate(data)]
fn checked() {
    println!("{}", Job(7));
}

fn main() {
    checked();
}
