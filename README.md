# tinted

Mark the sensitive functions in a Rust program and the functions that validate
their inputs, then check that no sensitive function can run without validation.

- [What it does](#what-it-does)
- [How it works](#how-it-works)
- [Testcases](#testcases)
- [Limitations](#limitations)
- [Requirements](#requirements)
- [License](#license)

## What it does

You annotate two kinds of function with the same identifier:

```rust
#[tinted::validate(transaction)]
fn verify_signatures(tx: &Transaction) -> Result<()> { /* ... */ }

#[tinted::sink(transaction)]
fn execute(tx: &Transaction) { /* ... */ }
```

`tinted-ir` then reads the compiled program and reports every way `execute` can
run that does not pass through `verify_signatures`:

```
[transaction]
  validate: verify_signatures
  sink: execute: FAIL: unchecked sink, 1 route(s)
    route 1:
      <- execute
      <- rpc::simulate
      <- main
```

It prints `OK` when every route into every sink is validated, and `FAIL` with
the unchecked routes otherwise. A sink with no validator gives a list of every
route into it, which is useful as an inventory.

## How it works

1. **Mark.** The attributes insert a marker call at the top of the annotated
   function, so the annotation survives into the compiled code.
2. **Build.** Compile the whole program to LLVM IR and link it into one module:

   ```sh
   CARGO_PROFILE_DEV_DEBUG=full \
   RUSTFLAGS="--emit=llvm-ir -C codegen-units=1 -C opt-level=1 -Cpanic=abort" \
   cargo build --target x86_64-unknown-linux-gnu --target-dir target-ir
   llvm-link-23 $(find target-ir -name '*.ll') -o all.bc
   ```

3. **Check.** Run the analysis on the linked module:

   ```sh
   (cd tinted-ir && cargo build --release)
   tinted-ir/target/release/tinted-ir [--known-issues FILE] all.bc
   ```

The check starts at each sink and walks upwards to `main`. A sink is validated
when every route to it is covered by one of these rules:

- **Callers.** Every caller is a validate function, is inlined from one, or is
  itself only reached through validated code.
- **Earlier calls.** In the function making the call, every path from its
  entry to the call first calls a validate function. This accepts
  `validate(x)?; sink(x);`, and rejects a sink reached on a branch that skips
  the validator.
- **Pointers.** Where a function on the route is used as a function pointer, in
  a vtable or as a callback, either the code taking its address is validated or
  every call through that pointer is.
- **Data.** Every argument of the call was produced by validated code. This is
  followed through locals, calls and channel messages: `std::sync::mpsc`,
  `crossbeam_channel`, `tokio::sync::{mpsc, oneshot, broadcast, watch}`,
  `futures::channel::{mpsc, oneshot}`, `flume` and `async-channel`.

To see the functions the analysis went through, add `--output DIR`. It writes
`DIR/index.txt` (every sink, its verdict and its file) and one text file per
sink: the sink, then its callers under it, then theirs, in the order they were
followed.

```
FAIL  [block] node::insert

FAIL  node::insert
ok      <- node::handle (src/main.rs:31)   [a validate function always runs first in the caller]
FAIL    <- node::import
FAIL      <- node::main
FAIL        <- main   [the program's entry point is reached without validation]
```

`ok` and `FAIL` say whether that function is only reached validated. `<-` is a
direct call and `<~` a call through a pointer. A note in brackets says why the
line stops there; `shown above` means the function's callers were already
listed. The arguments of generic types are left out (`<a::B<..>>::g`); the
sink's full name is on its own line. Every file starts with a `#` paragraph
that explains this format.

Known false positives go in a `--known-issues` file: a block of one function
name stops every route at that function, and a block of two names stops it at
that call. See `known-issues.txt`.

## Testcases

Each package in `testcases/` is a small program run through the same pipeline
as a real project. Its `src/main.rs` records what a perfect tool would say
(`want`) and what tinted says today (`today`).

```sh
bash testcases/run.sh              # every case
bash testcases/run.sh channel      # named cases only
python3 testcases/controls.py      # controls that must be rejected
```

The cases cover:

- **Channels:** data validated before a send on `std::sync::mpsc`,
  `crossbeam_channel`, tokio (`mpsc`, unbounded, `oneshot`, `broadcast`,
  `watch`), futures (`mpsc`, `oneshot`), `flume` (blocking and async) and
  `async-channel`, bounded channels, `select!`, and a second sender that puts
  unvalidated data on the same channel (must fail).
- **Deferred work:** queued closures, tokio tasks (spawn, blocking, local,
  lifecycle hooks, wakers, cancellation, shutdown), rayon jobs and thread
  spawn hooks.
- **Callbacks from prebuilt code:** `OnceLock`, `Hash` impls called by the hash
  table, signal handlers, `Display::fmt`, thread-local destructors.
- **Earlier calls:** a validator called next to the sink, before a loop, and
  in every arm of a dispatch function; and the shapes that must fail: one
  branch only, the wrong order, an unvalidated arm, and a second caller that
  never validates.
- **Pointer precision:** two function pointers, wakers, trait methods or
  thread-local accessors that share a type or signature, unused vtable slots
  and type-erased destructors.

Three cases (`skip_flag`, `order`, `wrong_data`) are marked `invariant`. A
perfect tool would fail them, and tinted passes them on purpose: they document
that validators are trusted (see below).

## Limitations

### False positives

Validated code that is reported as unvalidated.

- **Validation across an `.await`.** A validator called before an `.await`
  and a sink called after it are in different states of the compiled future,
  so the earlier-calls rule does not connect them.
- **Validation one call down.** A function that calls a helper which calls the
  validator is not credited; the validator must be called by the function
  itself, or be above the sink on the call chain.
- **Values returned through memory.** A validator's result that passes through
  a `Result`, an `Option` or an `await` is written to memory and read back, and
  the data rule loses it.
- **Method receivers.** `self` on a method sink is never validated data, so the
  data rule fails for it.
- **Data behind pointers.** Sink arguments are traced through locals and
  arguments, not fields. Data read through another pointer counts as
  unvalidated.
- **Pointers that cannot be followed.** A function pointer, vtable or trait
  object stored behind another pointer (a heap collection, a field of a boxed
  object), or passed to a call that is itself made through a pointer, counts as
  called from anywhere.
- **Channels are matched by message type.** Two channels carrying the same type
  are not told apart, so an unvalidated send on one fails a receive on the
  other.
- **Unrecognised channels.** Only the channels listed under the data rule are
  followed. Other channel crates and hand-written `Stream` implementations are
  not.

### False negatives

Unvalidated routes that are not reported.

- **Validators are trusted.** The tool checks that a validator is on the route.
  A validator that checks the wrong value or skips a branch behind a flag
  still passes. Under the earlier-calls rule, so does a sink reached after the
  validator reported a failure, and a sink that follows a validator call for
  unrelated data in the same function.
- **Code not reachable from `main`.** Library entry points that the binary
  never calls are not checked.
- **Callbacks kept by outside code.** A callback handed to the prebuilt
  standard library or a C library counts as called where it is handed over. One
  that is kept and called later (a panic hook, a signal handler, `pre_exec`) is
  not seen at that later point.
- **Calls from outside by name.** Only `main` is taken to be called from
  outside the module.
- **Unwind-only paths.** The module is built with `panic=abort`, so code that
  only runs while a panic unwinds is not in it.
- **Inline assembly and code loaded at run time.** A call made from an `asm!`
  block, into or out of a `dlopen`ed library, or in JIT-compiled code is not
  seen.
- **Code across a language boundary.** Calls into C or C++ are opaque.

## Requirements

- Rust (nightly for the test cases) on `x86_64-unknown-linux-gnu`.
- LLVM 23 development files and `llvm-link-23`. `LLVM_SYS_231_PREFIX` selects
  the LLVM that `tinted-ir` builds against (`/usr/lib/llvm-23` by default), and
  `LLVM_LINK` overrides the linker in the test runner.
- The program under test built at `opt-level=1` with full debug info and
  `-Cpanic=abort`, as in the build step above.
- Memory in proportion to the program: about 11 GB for a large validator
  binary.

## License

AGPL-3.0.
