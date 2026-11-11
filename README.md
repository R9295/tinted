# tinted

Source/validate/sink markers for Rust functions, and a check of them against
the program's LLVM IR.

## Analysis passes

Run `bash testcases/run.sh` to build and check every regression. This requires
nightly Rust plus LLVM 23 headers, libraries, `llvm-config-23`, `clang++-23`,
and `llvm-link-23`. `LLVM_CONFIG`, `CXX`, and `LLVM_LINK` can override the tools.

For an existing linked module:

```sh
bash passes/build.sh
target/passes/tinted-ir program.bc
```

The module is expected to be built by rustc at `opt-level=1` with debug info;
the analyzer runs no optimization of its own. It tracks pointer targets, vtable
fields and validation provenance from `main`. Where rustc inlined a summarized
library function, a task body or a validate function, the analyzer recognises
the inlined code from debug info and treats it as the call it came from. Call contexts stay separate, so a checked invocation cannot excuse
an unchecked invocation of the same function. Library summaries describe task
submission, polling, destruction, runtime callbacks and channel handoffs
(`std::sync::mpsc` and `crossbeam_channel`, including `select!`) without
expanding entire schedulers. Prebuilt callbacks are discovered from passed
function pointers and callback records rather than KCFI signature matching.

Validators remain trusted contracts. Their predicates, instruction order, and
choice of checked value are not verified. The three `invariant` cases below
explicitly retain that limitation; every other case must match `want`, not merely
reproduce its old failure. The older `tinted-check` / `tinted-resolve` call-graph
commands remain available for comparison.

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
| `erased_drop` | Type-erased destructors share a type id, with unused generated drop functions | OK | OK |
| `same_type_pointer` | Two function pointers of the same type | OK | OK |
| `thread_local_accessor` | Two thread locals of the same type share an accessor signature | OK | OK |
| `waker` | Two hand-written wakers share their function pointer types | OK | OK |
| `same_signature_method` | Two trait methods with the same signature | OK | OK |
| `same_trait_impl` | The same trait method on two different concrete types | OK | OK |
| `skip_flag` | A flag makes the validate function skip its check | FAIL | OK |
| `order` | Validation runs after the sink | FAIL | OK |
| `wrong_data` | One value is checked, another is executed | FAIL | OK |

The last three document the trusted invariants: their perfect-tool result is
FAIL, but these passes deliberately continue to trust the validator annotation.

## Limitations

These have no testcase.

- **Panic hooks.** `std::panic::set_hook` registers a callback invoked by
  prebuilt panic machinery, so its caller is missing. Hooks also run with
  `panic=abort`; this is separate from unwind-only paths.
- **Allocation-error hooks.** The nightly `std::alloc::set_alloc_error_hook`
  registers a function pointer invoked by prebuilt allocation-failure code.
- **Process-launch hooks.** On Unix, `CommandExt::pre_exec` registers a Rust
  closure invoked in the child process by prebuilt process-launch code.
- **Unwind-only paths.** KCFI requires `panic=abort`, so code that only runs
  while a panic unwinds, such as a destructor or the code after a
  `catch_unwind`, is not in the call graph.
- **Crates without KCFI.** A crate that rustc cannot compile with KCFI has to
  be built without it. Indirect calls in it, and into it, are not resolved.
- **Inline assembly.** A call made from an `asm!` block is not resolved.
- **Code loaded at run time.** A call into a `dlopen`ed library has no target
  in the call graph, and calls coming back from it have no caller.
- **Callbacks from C libraries.** A function that a C or C++ library calls
  back has no caller, because that code is not in the call graph. The
  `signal_handler` case shows the same thing with libc.
- **Function pointers cast to another type.** Indirect calls are matched by
  type, so a call through a pointer cast to a different function type is not
  matched to its real target.
