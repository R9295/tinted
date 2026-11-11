# tinted

Source/validate/sink markers for Rust functions, and a check of them against
the program's LLVM call graph.

## Known gaps

Each row is a small package in `testcases/` that tinted gets wrong. Run them
with `testcases/run.sh`.

| Case | Gap | A perfect tool says | tinted says |
|---|---|---|---|
| `channel` | Data validated before a channel send | OK | FAIL |
| `queued_closure` | Closure built under validation, run later by a worker | OK | FAIL |
| `tokio_task` | Task spawned under validation, run by a tokio worker | OK | FAIL |
| `tokio_spawn_blocking` | Validated closure run by a blocking pool worker | OK | FAIL |
| `tokio_spawn_local` | Validated non-Send task run by a `LocalSet` | OK | FAIL |
| `tokio_lifecycle` | Worker start, stop, park and unpark callbacks registered under validation | OK | FAIL |
| `tokio_waker` | Custom waker carrying validated data invoked by another task | OK | FAIL |
| `tokio_cancel` | Validated future's destructor run when its task is aborted | OK | FAIL |
| `tokio_shutdown` | Validated future's destructor run when its runtime shuts down | OK | FAIL |
| `thread_spawn_hook` | Parent and child spawn hooks invoked by prebuilt `std` | OK | FAIL |
| `rayon_job` | Closure given to a parallel iterator under validation | OK | FAIL |
| `std_callback` | Closure run by the prebuilt standard library (`OnceLock`) | OK | FAIL |
| `hash_callback` | `Hash` impl called back by the prebuilt hash table when it grows | OK | FAIL |
| `signal_handler` | Handler called by the kernel through libc | OK | FAIL |
| `format_callback` | `Display::fmt` callback run by prebuilt formatting code | OK | FAIL |
| `drop_thread_local` | Validated value's destructor run during thread-local cleanup | OK | FAIL |
| `unused_vtable_slot` | `dyn Fn` closure's extra vtable entries that nothing calls | OK | FAIL |
| `erased_drop` | Type-erased destructors share a type id, with unused generated drop functions | OK | FAIL |
| `same_type_pointer` | Two function pointers of the same type | OK | FAIL |
| `thread_local_accessor` | Two thread locals of the same type share an accessor signature | OK | FAIL |
| `waker` | Two hand-written wakers share their function pointer types | OK | FAIL |
| `same_signature_method` | Two trait methods with the same signature | OK | FAIL |
| `same_trait_impl` | The same trait method on two different concrete types | OK | FAIL |
| `skip_flag` | A flag makes the validate function skip its check | FAIL | OK |
| `order` | Validation runs after the sink | FAIL | OK |
| `wrong_data` | One value is checked, another is executed | FAIL | OK |

The last three are the dangerous kind: tinted passes something it should
reject.

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
