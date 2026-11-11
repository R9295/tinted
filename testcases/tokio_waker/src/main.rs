// Coverage: a custom waker handed to a runtime task. The receiver registers a
// waker carrying validated data; another task sends the message and invokes
// that waker, outside the validating future's call chain.
// want: OK
// today: OK
use std::{
    future::{Future, poll_fn},
    pin::Pin,
    sync::Arc,
    task::{Context, Wake, Waker},
};
use tokio::{runtime::Builder, sync::oneshot};

struct ForwardWake {
    data: u64,
    parent: Waker,
}

impl Wake for ForwardWake {
    fn wake(self: Arc<Self>) {
        execute(self.data);
        self.parent.wake_by_ref();
    }
}

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

#[tinted::validate(data)]
async fn process(x: u64) {
    let (tx, mut rx) = oneshot::channel();
    // On this current-thread runtime the sender cannot run until the
    // receiver has registered its waker and returned Pending.
    tokio::spawn(async move { tx.send(()).unwrap() });
    poll_fn(|cx| {
        let waker = Waker::from(Arc::new(ForwardWake {
            data: x,
            parent: cx.waker().clone(),
        }));
        Pin::new(&mut rx).poll(&mut Context::from_waker(&waker))
    })
    .await
    .unwrap();
}

fn main() {
    Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(process(7));
}
