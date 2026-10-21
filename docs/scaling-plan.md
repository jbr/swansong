# Scaling plan (2026-10-21)

Make swansong's hot paths scale with cores without changing its semantics. The contract is in
`docs/invariants.md`; tree semantics are in `docs/spec.md`.

**Why.** On HttpArena's 64-thread Zen 2 host, trillium's h1 throughput plateaus at
~0.6M req/s. The prime suspect is the per-request `swansong.interrupt(transport.read(..))` on the
server's shared root (`trillium-http` `src/conn/h1.rs`). h2 likewise takes a `guard()` on the
root for every stream. Full trillium context:
`~/code/trillium-bench/notes/many-core-brief-2026-10-21.md`.

## Baseline

Measured on a 4-core/8-thread i5 laptop. Numbers are Mops/s summed across threads.

```sh
cargo bench --bench scaling -- --millis 300
```

| op | 1 thread | 8 threads, shared | 8 threads, private | shared/private at 8 |
|---|---|---|---|---|
| interrupt-pending | 4.34 | 1.27 | 14.29 | 0.09 |
| interrupt-ready | 6.40 | 2.05 | 24.74 | 0.08 |
| guard | 5.24 | 1.86 | 22.82 | 0.08 |
| state | 236 | 977 | 1044 | 0.94 |
| child | 1.07 | 0.67 | 3.81 | 0.18 |

- **Allocation.** `cargo bench --bench ops` (divan) shows `interrupt_ready` allocating once per op
  (56 B, a boxed event listener) even though the wrapped future is already ready. Registration
  happens before the wrapped value is first polled.
- **Noise.** Laptop numbers are noisy under other load. Compare runs taken back to back.
- **Prototype.** A scratch version, not swansong code, kept per-thread shards of
  `CachePadded<{ AtomicBool, Mutex<Slab<Waker>> }>` with the interrupt holding an `Arc<Shard>`. It
  ran the pending→ready cycle at 18.8 Mops/s on 1 thread and 76 on 8, against 4.7 and 1.4 today.

## Design

### Shards

- **Ownership.** A node owns `Box<[Arc<CachePadded<Shard>>]>`.
  - Roots get `available_parallelism` shards, rounded up to a power of two and capped (256?).
  - Children get one shard; they are per connection and lightly contended. (Decision 5.)
- **Choosing a shard.** A thread picks its shard through a `thread_local!` index, assigned
  round-robin from a global counter on first use, taken modulo the shard count. TLS holds only
  the index, so a thread exiting needs no cleanup.
- **Contents of a shard.**
  - `stopped: AtomicBool`, written only at stop.
  - `waiters: Mutex<Slab<Waker>>`.
  - The phase 2 guard bookkeeping.
  - `node: Weak<Inner>`, used by `Interrupt::guarded` and for identity.

### Phase 1: interrupts

This is the trillium h1 bottleneck. trillium-websockets uses the same path once per message.

- **What `Interrupt` holds.** An `Arc<Shard>` plus an optional registration key, instead of
  `Weak<Inner>`. No path calls `Weak::upgrade` any more.
- **Stop check.** `shard.stopped.load(Relaxed)`.
- **Future/Stream poll.**
  1. If the flag is set, return terminal.
  2. Poll the wrapped value; if it's ready, return it. A ready value therefore never registers.
  3. On Pending, if not yet registered: lock the shard, re-check the flag (terminal if set),
     insert the waker.
  4. If already registered and `!will_wake`: lock the shard and replace the waker.
- **Drop.** If registered, lock the shard and remove the key.
- **`stop()`.**
  1. Set the node flag.
  2. For each shard: set `stopped`, lock, drain and wake everything.
  3. Propagate to children, as today.

  Because the flag is re-checked under the lock, a shard refuses registrations after a drain,
  so slab keys are never reused after stop.
- **Collected nodes.** `spec.md` §5.8 says interrupts on a collected child go terminal. To keep
  that, `Inner`'s `Drop` latches and drains its shards.
- **I/O and `Iterator` interrupts.** Check the flag through the shard, with no upgrade. Their
  semantics are unchanged: they stay passive. (Decision 1.)
- **`Interrupt::guarded()`** upgrades `shard.node`.
- **`PartialEq`** compares node identity with `Weak::ptr_eq` on `shard.node`, without upgrading.
- **Cold paths.** `ShuttingDown` and `ShutdownCompletion` stay on event-listener.
- **Precedence.** Keep "stop is checked first", which costs nothing. (Decision 2.)

### Phase 2: guards

This is h2's per-stream guard on the root.

- **Counting.**
  - A `Guard` records its home shard and increments `shard.count`, an atomic on the shard's own
    cache line.
  - Only a shard's 0→1 and 1→0 transitions touch the node's `packed` counter, which counts
    nonempty shards the way it counts nonempty children today. From there the existing ancestor
    propagation applies unchanged.
  - Shards act as implicit children in the existing hierarchy, so the existing transition
    reasoning carries over.
  - A guard decrements its home shard even when it's dropped on another thread.
- **Provenance.** A per-shard `Mutex<GuardRegistry>` replaces the node-wide one, and
  `guard_report` walks the shards. (Decision 3.)
- **`guard_count()`** sums shard counts and descendants, which is cold. `guard_count_subtree`
  currently reads the local count out of `packed` and has to change accordingly. (Decision 4.)

### Phase 3: warm paths and layout

- **`add_child`.** Use an amortized sweep threshold (as `GuardRegistry` does) instead of a
  `retain` on every insert.
- **Layout.** Keep `stopped`/`packed` off cache lines that hold refcounts or counters; h1 reads
  `state()` on every response.
- **`Swansong` clone/drop.** Unchanged; it's a warm path.

## Verification

- **Existing tests:** the `spec_*` and `stress_*` suites, every feature combination, and miri.
- **New loom models** (`docs/invariants.md` §7):
  - register vs. stop
  - waker replacement and task migration vs. drain
  - `Inner` drop vs. a pending interrupt
  - shard transitions vs. stop and completion
  - last guard drop vs. completion
- **Benches:**
  - `scaling`: shared/private ≥ ~0.7 at every thread count on the laptop.
  - `interrupt-ready`: zero allocations.
  - Then repeat on the Cherry Zen 4 box under `taskset`, with threads packed into one CCX and
    spread across CCXs.

## Defaults for the open decisions (pending Jacob's confirmation)

1. I/O and `Iterator` interrupts stay passive.
2. Stop wins when it coincides with readiness (current behaviour).
3. Guard provenance stays always-on, moved per shard. Revisit if divan shows it dominating.
4. `guard_count()` stays cold here. trillium-server-common's per-accept call gets fixed on the
   trillium side.
5. Roots are sharded; children have a single shard.

## Workflow

- **Process:** branch off `main`, use conventional commits, run the CI commands listed in
  `CLAUDE.md`, and keep `forbid(unsafe_code)`.
- **Public API:** no change is expected. Keep `Interrupt`, `Guard` and `Swansong` auto traits
  (`Send`/`Sync`/`Unpin`) identical, and assert that statically.
- **Downstream A/B:** trillium pulls swansong from crates.io. To compare, `trillium-bench`
  patches `swansong = { path = "../swansong" }`.
