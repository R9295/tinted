// Gap: rayon jobs. The closure is handed to a parallel iterator under the
// validate function, but a rayon worker may run it as a job through a
// function pointer, not called by it.
// want: OK
// today: FAIL
use rayon::prelude::*;

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

#[tinted::validate(data)]
fn process(items: &[u64]) {
    items.par_iter().for_each(|&x| execute(x));
}

fn main() {
    process(&[1, 2, 3]);
}
