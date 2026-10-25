# ADR 0058 — Gzip inflation memory budget

Status: Accepted — 2026-10-25

## Context

The J3 security review, item 6, found that 32 admitted gzip upload-pack requests
could retain 2 GiB of inflated data alone, exhausting the server unit's default
`MemoryMax` before compressed inputs, copies, Git children and runtime overhead.
ADR 0038 bounds individual inputs; the shared admission budget of 0040 and 0043
must also account for their aggregate allocations.

## Decision

- **Amends 0040 and 0043 — weighted public read admission.** The server-wide
  semaphore retains **32 permits**. Each `gzip` or `x-gzip` upload-pack request
  atomically acquires **eight permits** before reading its wire body. Plain
  upload-pack, advertisements, raw reads and browsing RPCs still cost one.
  Insufficient permits return HTTP **503** with `Retry-After: 1`, without
  reading the body or queuing work.
- All eight permits follow the existing upload-pack lifetime: through request
  buffering, inflation and supervised child completion or cancellation. Errors
  and cancelled handlers release the reservation with the input allocations.
- **Amends 0038 — retain the inflation allocation.** Successful inflation
  truncates and returns its output vector instead of copying it. The **16 MiB**
  wire and **64 MiB** inflated limits, growth policy and error statuses remain.
- Keep the server unit's default **`MemoryMax = "2G"`**. At most four gzip
  requests can retain 256 MiB of inflated buffers plus 64 MiB of compressed
  inputs: **320 MiB**, leaving **1728 MiB** for other memory. These requests use
  the entire shared read budget, so raw and browsing allocations cannot consume
  additional permits concurrently. Mixed workloads replace each gzip request
  with at most eight ordinary reads; even 32 plain upload-pack requests holding
  both their 16 MiB wire buffer and its copy account for **1 GiB**, leaving
  **1 GiB** for other allocations.

## Consequences

- Busy gzip clients may receive 503 sooner, while uncompressed read concurrency
  remains unchanged. No new dependency or configuration option is introduced.
- This bounds admitted gzip buffers, not total process RSS. Body collection,
  allocator overhead, transport buffers, database work and Git children also
  consume memory; `MemoryMax` remains the final process-group limit. Deployments
  lowering it must account for these fixed application limits.
- Regression coverage checks shared admission before body polling, both gzip
  encodings, mixed read exhaustion, atomic refusal and release on cancellation
  and malformed input. Existing inflation tests cover exact and exceeded caps.

## Rejected alternatives

- Increasing `MemoryMax`: raises the resource requirement without bounding the
  expensive work relative to other reads.
- A second semaphore or byte-budget allocator: unnecessary when the existing
  owned semaphore permit can carry the heavier reservation through cleanup.
- Reducing every read's concurrency: unnecessarily restricts ordinary reads.
