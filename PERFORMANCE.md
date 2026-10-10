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
