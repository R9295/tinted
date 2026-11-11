// Gap: vtable entries that nothing calls. A closure behind `dyn Fn` also gets
// `FnMut` and `FnOnce` entries in its vtable. They call the closure, nothing
// calls them, and the chain ends there.
// want: OK
// today: FAIL
use std::hint::black_box;

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

#[tinted::validate(data)]
fn run_checked(job: &dyn Fn(u64)) {
    job(1);
}

fn main() {
    run_checked(black_box(&|x| execute(x)));
}
