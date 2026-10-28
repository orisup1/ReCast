# Search latency

## Implemented changes

- `build.rs` now generates one- and two-letter prefix ranges for both embedded
  dictionaries and frequency lists. Dictionary membership, rank lookup, and
  completion/spelling prefix scans search only the relevant range. The four
  indexes add 23,328 bytes of read-only data, with no startup parsing or heap
  index. Other characters fall back to a wider search.
- The spelling alignment caches deletion costs and matching rule inputs for the
  typed word. Candidate alignments reuse this work. Changing the typed word
  rebuilds the cache; edit costs, rank limits, and confidence gates are unchanged.
- Short completion prefixes with exact candidates no longer generate hundreds
  of one-edit variants before immediately discarding them.
- Engine press/release handlers reuse the configuration snapshot already taken
  for that event when checking personalization, avoiding a second configuration
  lock and clone at those checks.

## Reproducing measurements

A local release run on Linux with an Intel Core i7-10875H measured the following
means using the original benchmark inputs and iteration counts:

| Search | Before | After | Time reduction |
| --- | ---: | ---: | ---: |
| English/Hebrew dictionary lookup pair | 1.171 µs | 0.892 µs | 24% |
| Completion candidates | 415.144 µs | 287.962 µs | 31% |
| English spelling correction | 3.596 ms | 2.799 ms | 22% |

The mixed-corpus lookup benchmark measured 2.427 µs per unindexed pair and
1.849 µs per indexed pair in the same binary, a 24% reduction. The broader
spelling distribution measured p50 2.396 ms, p95 7.239 ms, and maximum 7.991 ms;
that input set differs from the original mean benchmark and has no before
measurement. Results depend on CPU, workload, and cache state. The sandboxed
focus benchmark had no available focus replies, so its timings are not useful
evidence for desktop latency.

Run `make bench` for release microbenchmarks. The original benchmarks measure
means over five repeated inputs. The mixed dictionary benchmark samples both
corpora and compares indexed and unindexed searches within the same binary.
The spelling distribution benchmark warms its ten inputs, then reports p50, p95,
and maximum over 1,000 samples, including changed openings and long unknown words.
No benchmark asserts a wall-clock threshold.

These measurements cover search work, not the complete keyboard-to-screen path.
Physical key release, layout confirmation, and injection delivery also affect
visible latency. Injection timing settings remain unchanged.

## Engine capture benchmark and profiling

The shared state machine is split under `src/platform/engine/`. File size does
not itself establish a runtime bottleneck. The ignored `benchmark_engine_typing`
test exercises real press/release handlers with simulated focus and the English
word `keyboard`. It reports letter and Space latency separately after 100 warmup
words, with 20,000 measured words and personalization disabled. Letter samples
are averages across the word's eight press/release pairs. Space includes automatic
planning for an unchanged known word. Assertions check that no replacement was
scheduled and that the word buffer was cleared. It does not measure native focus
queries, misspelling search, layout changes, or on-screen injection.

```sh
cargo test --locked --release benchmark_engine_typing -- --ignored --nocapture --test-threads=1
```

Three consecutive local Apple Silicon macOS release runs measured letter-pair
p50 values of 312–333 ns and Space-pair p50 values of 11.167–11.917 µs. These are
synthetic steady-state measurements, not native keyboard-to-screen latency or a
controlled before/after comparison. A CPU flamegraph was not captured in this
environment: `cargo-flamegraph` and `xctrace` were unavailable.

For CPU stack samples, install `cargo-flamegraph` and its platform backend:
Linux needs `perf`; macOS needs Xcode's `xctrace`. Follow the
[upstream setup instructions](https://github.com/flamegraph-rs/flamegraph),
including Linux linker flags for lld/mold and local profiler permissions.
Keep symbols for the profiling build without changing shipping settings:

```sh
cargo install flamegraph --locked
CARGO_PROFILE_RELEASE_DEBUG=true CARGO_PROFILE_RELEASE_STRIP=false \
  cargo flamegraph --unit-test recast -o target/engine-flamegraph.svg -- \
  platform::engine::tests::benchmark_engine_typing --exact --ignored --nocapture --test-threads=1
```

Repeat with `benchmarks::benchmark_spelling_correction` or
`benchmarks::benchmark_completion_candidates` to inspect slower candidate searches.
The capture benchmark uses no subprocess, so the profiler samples the handlers
directly. Profile actual desktop typing separately for OS-query and injection
costs. CPU flamegraphs locate sampled CPU work; they do not count heap allocations.

## Dictionary memory and correction scheduling

Dictionary blobs remain embedded as static strings with build-time prefix indexes.
They are not parsed or copied into heap dictionaries at startup. Read-only binary
data already benefits from OS paging; a separate mmap would add external files,
deployment/version matching, and failure handling without establishing a memory
saving. Measure resident memory and page faults before changing this format.
Apple describes [executable paging and virtual memory](https://developer.apple.com/library/archive/documentation/Performance/Conceptual/ManagingMemory/Articles/AboutMemory.html).

Automatic correction starts only on Space, after a word is finished. Enter clears
context without an automatic rewrite. Continued typing during replacement is
buffered and replayed, with cancellation for stale focus or cursor context.
Adding an idle debounce would change when edits can safely land after the cursor
has advanced; it requires a measured problem and coverage for replay, undo, focus,
and layout races. No debounce or injection delay was added in the module split.

## Further opportunities

1. **Completion variant allocation.** `edited_prefixes` clones a character vector
   for each insertion, substitution, deletion, and transposition, then allocates
   a string. Reusing one scratch vector or constructing variants from borrowed
   string slices could remove the temporary vector allocations. Keep Unicode
   boundaries and candidate deduplication intact; benchmark Hebrew as well as
   English.
2. **Typing-state lock duration.** `Engine::word_finished` runs the planner while
   holding the state mutex. Moving pure search onto an owned snapshot could let
   key releases and cancellation proceed during slower searches. This requires
   separating layout changes from planning and validating revision, generation,
   focus, and modifiers before committing a result. Existing fresh focus checks
   must remain immediately before injection.
3. **Phrase lookup allocation.** `PhraseContext::boost` clones the previous word
   and allocates the candidate to look up a tuple key. Nested maps with borrowed
   string lookups could avoid those allocations. Preserve the 256-pair limit,
   saturating counts, and context reset behavior.

## Correctness checks

Tests verify every embedded dictionary entry and frequency rank, compare all
indexed one- and two-letter frequency scans against unindexed searches, and run
the correction accuracy corpus. Full regression tests and Clippy checks cover
the shared code; native CI remains responsible for macOS and Windows runtime
behavior.
