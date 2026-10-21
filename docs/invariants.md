# Swansong invariants and cost contract

What any implementation of swansong must uphold, independent of data structure. `spec.md`
covers the semantics of children/trees; this document covers the signal, interrupt and guard
contract, and the steady-state cost rules under many-core contention. Where they overlap,
`spec.md` governs tree semantics.

Draft, 2026-10-21. Items marked **Decision** are open.

## 1. Workload assumptions

- **Shutdown is rare.** It happens at most once per node, and usually once per process
  lifetime. Steady state is effectively all of runtime, so any cost moved from the steady
  state onto shutdown is a win.
- **Hot operations run per request, stream or message, on every core.** They are interrupt
  create/poll/drop, guard create/drop, and `state()` checks. Rates of 10⁶–10⁷/s per process
  across 64+ threads are expected.
- **Typical tree shape.** One long-lived root is shared by every connection on every core,
  with many short-lived children (one per connection).
- **Tasks move.** They migrate between threads on work-stealing runtimes. Other runtimes are
  thread-per-core. Both must be correct, and neither may be slow.

## 2. Safety (must always hold)

- **S1 Monotonic stop.** `stopped` latches. Stop is idempotent and never reverts.
- **S2 No spurious interruption.** An Interrupt yields its terminal value only if its node is
  stopped (explicit `shut_down`, propagation from an ancestor, or the last root handle
  dropped) or its node has been collected (`spec.md` §5.8).
- **S3 Completion safety.** A `ShutdownCompletion` never resolves while a Guard is alive in
  its subset, if that Guard's creation happened-before the completion's final emptiness check.
  Guards racing that check may land on either side (`spec.md` §5.7).
- **S4 Completion is monotonic.** A resolved completion stays resolved.
- **S5 Exact when quiescent.** With no guard operations in flight, `guard_count()` and
  `state()` are exact. Under concurrency they may be momentarily stale.

## 3. Liveness: eventual, not atomic

- **L1 Eventual wake.** Every `Interrupt<Future>` / `Interrupt<Stream>` that is pending at any
  point after its node is stopped is woken at least once after the stop. The first poll that
  begins after such a wake observes the stop.
- **L2 A racing poll may miss the stop.** A poll that overlaps `stop()` may return Pending or
  the wrapped value without observing the stop, provided L1 still holds for it: if it returns
  Pending, a later wake is guaranteed.
- **L3 Eventual completion.** Once a node is stopped and its subset is empty, every pending
  `ShutdownCompletion` and `ShuttingDown` is eventually woken and resolves.
- **L4 Migration-safe.** A registration made on one thread and updated, woken or dropped on
  another stays correct.

### What L1–L4 permit

- Steady-state stop checks may be `Relaxed` loads. No `SeqCst` on the hot path.
- Ordering between "register for wake" and "stop" may come from a lock on the registration's
  own shard rather than from global `SeqCst` ordering. The stop path sets the flag, then
  locks and drains each shard. A registration that takes the shard lock after the drain is
  guaranteed to see the flag.
- Wakes may be spurious, duplicated, batched, unordered, and issued from any thread.
- **Lazy registration.** An Interrupt registers only after the wrapped value returns Pending,
  so an immediately-ready wrapped value never registers.
- **Persistent registration.** A registration may persist across polls, replacing the waker
  only when `!will_wake`. A poll that follows any wake must re-check the flag. Because the
  waker contract orders wake before the next poll, a `Relaxed` check suffices there.
- After stop, a registry may refuse new registrations; the registering poll then returns
  terminal. This also rules out slot reuse (ABA) after a drain.

### What may not be relaxed

- Discarding a registration without waking it.
- Returning Pending after stop with no wake guaranteed to follow.
- Waking a task whose next poll cannot observe the stop (it would re-register and could
  sleep forever).

## 4. Per-wrapper semantics

| wrapper | stop behaviour |
|---|---|
| `Future`, `Stream` | proactive: L1 applies |
| `AsyncRead`, `AsyncWrite`, `AsyncBufRead`, `Iterator` | passive: observed at the next operation; a pending read or write is not woken |

- **Decision:** keep the I/O wrappers passive? An idle keep-alive read parked in
  `Interrupt<AsyncRead>` never notices shutdown until data arrives, which is why trillium
  wraps the read *future* instead.
- **Decision:** when the stop and the wrapped value's readiness coincide in one poll, may
  either win? Today the stop wins because it is checked first. L2 allows either.

## 5. Cost contract

*Steady state*: the node and its ancestors are running, and no diagnostic call is in progress.

*Shared write*: any store, read-modify-write or lock acquisition on a cache line that other
threads also write in steady state.

### Hot paths

These run per request, stream or message, and must be as close to transparent as possible:

- Interrupt create, every poll (all wrapper kinds), drop
- Guard create (including `Guard::clone`, `Swansong::guarded`, `Interrupt::guarded`), drop
- `state()` and stop-flag checks

The rules:

- **H1 No shared writes in steady state.** The only exception is a bounded fraction of calls
  that does not grow with thread count or request rate under sustained load, for example a
  per-shard empty↔nonempty transition. Writes to memory owned by the calling thread's shard
  are fine, whether atomics or an uncontended lock. Occasional cross-thread access caused by
  task migration is fine.
- **H2 No reads of contended lines.** The stop flag sits on a line that is written only at
  stop, never next to counters or refcounts.
- **H3 Flat cost.** Per-operation cost does not grow with the number of threads, live guards,
  live interrupts, or children.
- **H4 No per-operation allocation** that doesn't amortize away (reusing slab slots is fine).
  **Decision:** is per-guard provenance (an `Arc<GuardInfo>` plus `Instant::now()`) acceptable
  in steady state, or should it become opt-in?

### Warm paths

These run per connection: `child()` and child drop, `Swansong` clone/drop, and creating
`ShuttingDown` / `ShutdownCompletion`.

- They may write shared lines.
- They must be amortized O(1).
- They must never hold a shared lock for O(n).

### Cold paths

These run at shutdown or for diagnostics: `shut_down()` and its propagation, the last root
handle dropping, completion resolution, `guard_count()` and `guard_report()`.

- They may be O(tree + guards + waiters + threads) and may take any locks.
- **Decision:** `trillium-server-common` calls `guard_count()` on every accept when
  `max_connections` is set. Either make a node's own count O(shards), or treat it as cold and
  change the caller.

### Memory

- Children are per connection, so their fixed overhead stays small.
- Per-thread structures (shards) live on roots, on nodes that opt in, or are allocated
  lazily. **Decision:** which of these?

## 6. Known violations in 0.3.6

These are the targets of the redesign.

**`Interrupt<Future/Stream>`, every pending poll:**
- registers on the node's single `stop_event`;
- event-listener then boxes a listener, clones an `Arc`, and makes three trips through one
  `std::sync::Mutex` (insert, register, remove);
- around four `Weak::upgrade`s (CAS plus decrement) hit the node's shared strong count.

**`Interrupt::new` and drop:** `Weak` downgrade and drop act on the shared weak count.

**I/O and `Iterator` interrupts:** a `Weak::upgrade` on every read, write or `next()`. That
is a shared read-modify-write per I/O call.

**Guard create/drop:**
- the node-wide `guard_registry` mutex, which also sweeps O(live guards) while held;
- a read-modify-write on `packed`;
- an `Arc` clone and drop of `Inner`.

**`state()`:** reads `stopped` and `packed`, which share lines with counters and refcounts.
That is false sharing on every check.

**`child()`:** an O(live children) `retain` under the parent's `children` mutex on every
creation.

**S3 is violated across levels.** `increment_guard` increments the local count and only then
propagates an empty→nonempty transition to the ancestors, and a Guard that finds the count
already nonzero returns at once. So when two threads create the first Guards on an empty
child, the second can return before the first has made the child count at the root. If the
second thread then calls `root.shut_down()`, the returned completion can resolve while that
Guard, created before the call, is alive. A loom model of the protocol finds it, and a stress
test against the published crate hit it about once per 100k rounds.

### Measured

4-core laptop, single die; numbers are total ops/s across all threads.

| threads | interrupt pending→ready cycle, shared root | guard create+drop, shared root | guard create+drop, private per thread |
|---|---|---|---|
| 1 | 4.7M | 6.4M | 6.1M |
| 2 | 2.1M | 2.8M | 9.3M |
| 4 | 1.5M | 2.3M | 15.3M |
| 8 | 1.4M | 1.8M | 24.6M |

On the HttpArena host (Zen 2, 64 threads across 8 CCXs), trillium's h1 throughput plateaus at
~0.6M req/s, with one shared-root interrupt per request. A prototype with per-thread shards
(`Mutex<Slab<Waker>>` and an `Arc<Shard>` holding its own flag) reached 18.8M ops/s on one
thread and 76M on eight.

## 7. Verification

- **loom:**
  - register vs. stop (L1, L2)
  - drain vs. migration and waker replacement (L4)
  - shard empty↔nonempty transitions vs. stop (S3)
  - last guard drop vs. completion (L3)
- **miri / TSAN stress:** as existing.
- **Scaling bench:** ops/s against thread count for each hot operation, shared node against
  per-thread node, with threads packed into one CCX and spread across CCXs. Acceptance:
  shared stays within ~1.5× of private at every thread count.
