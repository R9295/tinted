# tinted

Source/validate/sink markers for Rust functions, and a check of them against
the program's LLVM IR.

## tinted-ir

`tinted-ir` (a Rust program in `tinted-ir/`, built against LLVM 23 through
`llvm-sys`) reads the linked module and checks every sink. It replaces the
`tinted-check` / `tinted-resolve` call-graph commands.

Run `bash testcases/run.sh` to build and check every regression, and then
`python3 testcases/controls.py` for the controls that must be rejected. This
requires nightly Rust, the LLVM 23 development files and `llvm-link-23`
(`LLVM_LINK` overrides it; `LLVM_SYS_231_PREFIX` selects the LLVM to build
against, `/usr/lib/llvm-23` by default).

For an existing linked module:

```sh
(cd tinted-ir && cargo build --release)
tinted-ir/target/release/tinted-ir [--known-issues FILE] program.bc
```

The module is expected to be built by rustc at `opt-level=1` with full debug
info and `-Cpanic=abort` (the flags in `testcases/run.sh`), and linked with
`llvm-link`; nothing is optimized or run. `-Zsanitizer=kcfi` is no longer
needed: indirect calls are resolved by following the pointer, not by type id. The check starts at the sinks and works upwards,
so its cost depends on what is above the sinks, not on the size of the program.
A sink is checked when every way it can run is validated:

- **Callers.** Every direct caller must be a validate function, be code
  inlined from one (recognised from debug info), or itself only run validated.
- **Pointers.** Where the address of a function on the chain is taken (a
  function pointer, a vtable, a callback record), either the code taking it
  must be validated, or every call made through that pointer must be. The
  pointer is followed forward from where it is taken, through locals,
  arguments, returns and fields, and only when the first does not hold. A
  pointer handed to code outside the module counts as called where it is
  handed over. A pointer that cannot be followed counts as called anywhere.
- **Data.** A call of a sink is also checked when all of its arguments were
  produced by validated code. Arguments are traced back through locals, calls
  and channel receives (`std::sync::mpsc` and `crossbeam_channel`, including
  `select!`); a receive is checked when every send of that message type is.

Only code reachable from `main` counts. A failure prints the chain from the
sink up to where it ends. `--known-issues FILE` lists known false positives
(see `known-issues.txt`): blocks separated by blank lines, one demangled
function name per line as printed in a chain. A block of one name stops every
chain at that function, and a block of two stops it at that call.

Validators remain trusted contracts. Their predicates, instruction order, and
choice of checked value are not verified. The three `invariant` cases below
explicitly retain that limitation; every other case must match `want`.
`source` annotations are accepted but not needed: every chain into a sink is
checked, wherever it starts.

## Regression cases

Each row is a package in `testcases/`. The three invariant cases are identified
in their source headers and reported separately by the runner.

| Case | Coverage | A perfect tool says | tinted says |
|---|---|---|---|
| `channel` | Data validated before a channel send | OK | OK |
| `crossbeam_channel` | Data validated before a crossbeam channel send | OK | OK |
| `crossbeam_bounded` | Bounded crossbeam channel through `try_send` and `recv_timeout` | OK | OK |
| `crossbeam_select` | Receiver waits on two validated channels with `select!` | OK | OK |
| `crossbeam_unchecked_sender` | A second sender puts unvalidated data on a crossbeam channel | FAIL | FAIL |
| `crossbeam_select_unchecked` | One of two channels in a `select!` carries unvalidated data | FAIL | FAIL |
| `queued_closure` | Closure built under validation, run later by a worker | OK | OK |
| `tokio_task` | Task spawned under validation, run by a tokio worker | OK | OK |
| `tokio_spawn_blocking` | Validated closure run by a blocking pool worker | OK | OK |
| `tokio_spawn_local` | Validated non-Send task run by a `LocalSet` | OK | OK |
| `tokio_lifecycle` | Worker start, stop, park and unpark callbacks registered under validation | OK | OK |
| `tokio_waker` | Custom waker carrying validated data invoked by another task | OK | OK |
| `tokio_cancel` | Validated future's destructor run when its task is aborted | OK | OK |
| `tokio_shutdown` | Validated future's destructor run when its runtime shuts down | OK | OK |
| `thread_spawn_hook` | Parent and child spawn hooks invoked by prebuilt `std` | OK | OK |
| `rayon_job` | Closure given to a parallel iterator under validation | OK | OK |
| `std_callback` | Closure run by the prebuilt standard library (`OnceLock`) | OK | OK |
| `hash_callback` | `Hash` impl called back by the prebuilt hash table when it grows | OK | OK |
| `signal_handler` | Handler called by the kernel through libc | OK | OK |
| `format_callback` | `Display::fmt` callback run by prebuilt formatting code | OK | OK |
| `drop_thread_local` | Validated value's destructor run during thread-local cleanup | OK | OK |
| `unused_vtable_slot` | `dyn Fn` closure's extra vtable entries that nothing calls | OK | OK |
| `erased_drop` | Type-erased destructors of two types, with unused generated drop functions | OK | OK |
| `same_type_pointer` | Two function pointers of the same type | OK | OK |
| `thread_local_accessor` | Two thread locals of the same type share an accessor signature | OK | OK |
| `waker` | Two hand-written wakers share their function pointer types | OK | OK |
| `same_signature_method` | Two trait methods with the same signature | OK | OK |
| `same_trait_impl` | The same trait method on two different concrete types | OK | OK |
| `skip_flag` | A flag makes the validate function skip its check | FAIL | OK |
| `order` | Validation runs after the sink | FAIL | OK |
| `wrong_data` | One value is checked, another is executed | FAIL | OK |

The last three document the trusted invariants: their perfect-tool result is
FAIL, but tinted-ir deliberately continues to trust the validator annotation.

## Limitations

These have no testcase.

- **Pointers that cannot be followed.** A function pointer or vtable stored
  in memory reached through another pointer (a heap collection, a field of a
  boxed object), or passed to a call that is itself made through a pointer,
  counts as called anywhere. Unless the code that took the address is
  validated, the sink is reported. This errs towards false positives.
- **Callbacks kept by outside code.** A callback handed to the prebuilt
  standard library or a C library counts as called where it is handed over.
  One that is kept and called later from somewhere else (a panic hook, a
  signal handler, `pre_exec`) is not seen at that later point.
- **Channels are matched by message type.** Two channels carrying the same
  type are not told apart, so an unvalidated send on one fails a receive on
  the other.
- **Data behind pointers.** Sink arguments are traced through locals and
  arguments, without regard to fields. Data read from memory reached through
  another pointer is not traced and counts as unvalidated.
- **Calls from outside by name.** Only `main` is taken to be called from
  outside the module. A function that outside code calls by its symbol name
  has no caller.
- **Unwind-only paths.** The module is built with `panic=abort`, so code that
  only runs while a panic unwinds is not in it.
- **Inline assembly and code loaded at run time.** A call made from an `asm!`
  block, or into or out of a `dlopen`ed library, is not seen.
