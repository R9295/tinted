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
| `rayon_job` | Closure given to a parallel iterator under validation | OK | FAIL |
| `std_callback` | Closure run by the prebuilt standard library (`OnceLock`) | OK | FAIL |
| `unused_vtable_slot` | `dyn Fn` closure's extra vtable entries that nothing calls | OK | FAIL |
| `same_type_pointer` | Two function pointers of the same type | OK | FAIL |
| `same_signature_method` | Two trait methods with the same signature | OK | FAIL |
| `skip_flag` | A flag makes the validate function skip its check | FAIL | OK |
| `order` | Validation runs after the sink | FAIL | OK |
| `wrong_data` | One value is checked, another is executed | FAIL | OK |

The last three are the dangerous kind: tinted passes something it should
reject.
