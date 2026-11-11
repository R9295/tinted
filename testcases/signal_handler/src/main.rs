// Coverage: callbacks from C and the operating system. The handler is installed
// and triggered under validation, but it is called by the kernel through
// libc, which is not in the call graph, so it has no caller.
// want: OK
// today: OK
use std::hint::black_box;

#[tinted::sink(data)]
fn execute(x: u64) {
    black_box(x);
}

extern "C" fn handler(_signal: libc::c_int) {
    execute(7);
}

#[tinted::validate(data)]
fn checked() {
    unsafe {
        libc::signal(libc::SIGUSR1, handler as extern "C" fn(libc::c_int) as libc::sighandler_t);
        libc::raise(libc::SIGUSR1);
    }
}

fn main() {
    checked();
}
