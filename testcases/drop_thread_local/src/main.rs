// Gap: thread-local cleanup. A value is stored under validation, but its
// destructor runs later at thread exit. The graph does not connect cleanup
// to the validated initialization and also includes unused storage drop glue.
// want: OK
// today: FAIL
use std::{cell::RefCell, hint::black_box, thread};

struct Job(u64);

impl Drop for Job {
    fn drop(&mut self) {
        execute(self.0);
    }
}

thread_local! {
    static JOB: RefCell<Option<Job>> = const { RefCell::new(None) };
}

#[tinted::sink(data)]
fn execute(x: u64) {
    black_box(x);
}

#[tinted::validate(data)]
fn checked() {
    JOB.with(|slot| *slot.borrow_mut() = Some(Job(7)));
}

fn main() {
    thread::spawn(checked).join().unwrap();
}
