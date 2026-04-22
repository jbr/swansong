# Swansong specification

This document describes *what* Swansong is — its domain, its semantics, and
its invariants — and concludes with a note on the chosen implementation
architecture for the hierarchical extension.

---

## 1. Domain

Swansong coordinates the **graceful shutdown of a unit of asynchronous or
concurrent work**. It exists because the two endpoints of shutdown —
"stop accepting new work" and "wait for in-progress work to finish" — have
to be separable. A naive shutdown that just cancels everything loses work
that was almost-done; a naive shutdown that just waits never terminates.

Three conceptual primitives:

- **Shutdown signal.** A one-way, monotonic flip from *running* to *stopped*.
  Anyone watching can observe the flip.
- **Guard.** An RAII handle representing a piece of *committed, in-progress
  work*. While any Guard exists, shutdown is not complete.
- **Interrupt.** A wrapper around a future, stream, iterator, or async I/O
  handle that reacts to the shutdown signal by returning its terminal value
  (None / Ready / EOF / 0-byte) at the next appropriate boundary.

The key separation: guards delay *completion* of shutdown; they do not delay
*propagation* of the signal. Interrupts see the signal immediately.

## 2. Conceptual model

**A `Swansong` names a subset of Guards.** The subset is the unit over which
shutdown is initiated (`shut_down`) and observed (`ShutdownCompletion`,
`state`, `guard_count`). Guards are the atomic primitive; Swansongs are
named views over groups of guards.

In the current API, subsets are organized as a **tree**: each child Swansong
names a subset strictly contained within its parent's subset. A Guard
created via a child is a member of the child's subset and, transitively,
every ancestor's subset. Siblings name disjoint subsets.

This spec does not preclude other organizations of subsets (overlapping,
non-hierarchical) but does not define an API for them. The tree is what the
current API exposes; the architecture is not fundamentally married to it.

Consequences that fall out of this model:

- **A Swansong is not work.** It is a name for a set of guards. Creating a
  Swansong commits no work; only creating a Guard does.
- **Losing the handle to a subset is transparent.** If the last handle to
  a non-root subset is dropped while guards remain, those guards are still
  members of the ancestor subsets that continue to observe them. The
  subset becomes unobservable by name, but the guards it contained are
  unaffected.
- **The root is the termination case.** The root subset has no ancestor to
  fall back to. If the user drops all handles to the root with no
  explicit shutdown, we treat it as implicit shutdown to prevent
  observers waiting indefinitely on something no one governs.

## 3. Single-Swansong specification (behavior)

### 3.1 States

A Swansong is in exactly one of three states:

- **Running** — not stopped.
- **ShuttingDown** — stopped, but at least one Guard in its subset.
- **Complete** — stopped, and no Guards in its subset.

State transitions are monotonic with respect to `stopped`:
`Running → ShuttingDown → Complete`, or `Running → Complete` if shutdown is
signaled while no guards exist. A ShutdownCompletion that has resolved does
not un-resolve; subsequent guard creation can make `state()` report
ShuttingDown again, but existing completion futures remain Ready.

### 3.2 Operations

- **`Swansong::new()`** — create a fresh Running root Swansong.
- **Clone** — produces another handle to the same subset. Equality is
  identity. Does not affect state.
- **`guard() -> Guard`** — produces a Guard in this subset. Dropping the
  Guard releases it. Guards are clonable; each clone is independently
  counted.
- **`guarded(T) -> Guarded<T>`** — attaches a Guard to T; dropping the
  wrapper drops the Guard. Trait impls (Future, Stream, AsyncRead/Write,
  Deref) are transparent.
- **`interrupt(T) -> Interrupt<T>`** — wraps T with shutdown-awareness.
  Trait impls check the stop signal before delegating: Future returns
  `None` (yielding `Option<Output>`), Stream yields `None`, Iterator yields
  `None`, AsyncRead/Write return `Ok(0)` (EOF / zero write). Interrupt can
  optionally also hold a Guard via `Interrupt::guarded`.
- **`shut_down() -> ShutdownCompletion`** — signals stop (idempotent) and
  returns a future/blocking handle for Complete.
- **`state() -> ShutdownState`** — observe current state.
- **`guard_count() -> usize`** — observe number of Guards in this subset
  (see Section 5.4 for hierarchical semantics).
- **`IntoFuture for Swansong`** — awaiting a Swansong handle is equivalent
  to awaiting its `ShutdownCompletion` without directly signaling stop.
- **`Drop for Swansong`** — see 3.3 and 5.8.

### 3.3 Handle-drop semantics (root)

Dropping the *last* clone of a root Swansong signals stop, as if
`shut_down()` had been called. This is a defensive default: if no one
holds the root handle, no one is governing the subset, and Interrupts
observing it should not poll indefinitely.

(Non-root child handles have different drop semantics; see Section 5.8.)

### 3.4 Guard lifetime

A Guard holds its originating subset's coordination state alive for the
Guard's lifetime. An outstanding Guard prevents that state from being
collected even when every `Swansong` clone naming the subset has been
dropped. This ensures that the Guard's drop still correctly decrements
its subset's counters and any counters of ancestor subsets.

In the non-hierarchical case this has no observable effect — without a
Swansong handle, nothing is watching the orphaned Guard. In the
hierarchical case, ancestor subsets are watching, and the Guard's
correctness depends on its accounting reaching them.

### 3.5 Invariants

- **Monotonicity.** `stopped` is latching.
- **Signal visibility precedes wait.** When `shut_down()` is called, any
  Interrupt polled afterward observes the stop; any ShutdownCompletion
  awaited afterward will eventually resolve (modulo the guard discipline).
- **Liveness.** If every Guard is eventually dropped and every Interrupt
  is eventually polled, `shut_down().await` terminates.
- **Hot paths are lock-free.** Guard create/drop, Interrupt poll/read, and
  stop-signal observation must not take a Mutex on the critical path.

## 4. Single-Swansong implementation (descriptive)

The implementation shape is **atomics for the hot path, events for the
wake-up, reference-counting so that state lives as long as any observer
or member**.

Per-node state lives in `Arc<Inner>` containing:

- `stopped: AtomicBool` — the stop latch.
- `local_guards: AtomicUsize` — Guards held directly in this subset.
- `stop_event: Event` — notified on the stopped=false→true transition.
- `zero_event: Event` — notified when the subset's completion condition
  holds while stopped.

All critical atomic operations use SeqCst ordering — the implementation
prioritizes correctness over the marginal throughput of weaker orderings.

The `event-listener` crate provides async and blocking waits on the two
events. A `Listener` is registered *before* a second check of the
condition, to close the race where the condition becomes true between
first check and registration.

- **Guards hold `Arc<Inner>`** to their originating node (see 3.4).
- **Interrupts hold `Weak<Inner>`** so that user handles govern lifetime.
  If `Weak::upgrade` fails, the Interrupt returns its terminal value; this
  is how the non-hierarchical case handles an orphaned Interrupt after the
  last Swansong is dropped.
- **ShutdownCompletion holds `Arc<Inner>`** so the future or blocking call
  can resolve even if the user drops all Swansong handles.

The `Drop for Swansong` implementation decrements an explicit
`swansong_count: AtomicUsize` on `Inner` and, for root nodes, triggers
`stop()` when the count reaches zero. `Arc::strong_count` is not used for
this purpose because Guards and ShutdownCompletions also hold strong
references.

## 5. Hierarchical extension (semantic spec)

### 5.1 Operation

- **`parent.child() -> Swansong`** — creates a new Swansong whose subset is
  nested strictly inside `parent`'s subset. The returned handle satisfies
  the entire single-Swansong spec (Section 3) in its own right.

Nesting is arbitrary. "Tree" refers to the structure rooted at a node,
descending through all transitively-created children.

### 5.2 Core principle: guards are the only units of work

A Swansong is not work; it is a name for a subset. Consequences:

- **Creating a child is transparent to all ancestors.** Immediately after
  `parent.child()`, the parent's observable state is unchanged: no new
  guards, no delay to completion, no change in `state()`, no change in
  `guard_count()`.
- **Dropping a child handle without outstanding guards is also
  transparent.** If no Guard was ever created on the child (or all have
  been dropped), dropping the last child handle leaves the parent's
  observable state unchanged. Equivalently: `parent.child().guarded(X)`
  with an immediately-discarded child handle is semantically identical to
  `parent.guarded(X)`.
- **A child that never registers a guard never delays any ancestor.** If
  `parent.shut_down()` is called while the child has zero guards anywhere
  in its own subtree, the parent completes without waiting for the child.
- **Children do not count as guards.** Just as a sibling or clone Swansong
  does not count as a guard, neither does a child.

### 5.3 Propagation semantics

- **Shutdown propagates downward.** `parent.shut_down()` causes every
  descendant swansong (direct and transitive) to transition to stopped.
  Every Interrupt anywhere in the subtree will observe stop at its next
  poll.
- **Shutdown does not propagate upward.** `child.shut_down()` affects only
  that child's subtree. The parent's `stopped` and `state()` are
  unaffected.
- **Shutdown propagation is not atomic.** `parent.shut_down()` returns a
  `ShutdownCompletion` and initiates propagation; the user-facing
  guarantee is that the returned completion resolves when the entire
  subtree has drained, not that propagation has completed before
  `shut_down()` returns. Callers who need a guaranteed drain await the
  completion.
- **Completion aggregates upward.** A node is Complete iff it is stopped
  AND its subset (itself plus all descendants transitively) contains no
  Guards.
- **Siblings are independent.** Shutting down or completing one child
  does not affect another.

### 5.4 Observation semantics

- **`guard_count()`** at any node returns the total number of *real*
  Guards anywhere in that node's subtree. Not including descendant
  Swansongs themselves, not including any structural bookkeeping, not
  including Guards from unrelated trees. Just user-visible Guard
  instances in this subset.
- **`state()`** at any node reflects its subtree:
  - Running if not stopped.
  - ShuttingDown if stopped and `guard_count() > 0`.
  - Complete if stopped and `guard_count() == 0`.
- **`shut_down().await`** at any node resolves when that node is Complete
  per the above.

### 5.5 Lifetime and collectibility

- A child Swansong is a distinct identity from its parent. `Eq` is identity.
  `parent == parent.child()` is false.
- A node's `Inner` is kept alive while any of the following hold: a live
  Swansong clone of that node, an outstanding Guard whose home is that
  node, an unresolved ShutdownCompletion for that node, or any live
  descendant `Inner`. The last clause is a consequence of the Arc-based
  ancestor chain (see 8): descendants hold strong references to every
  ancestor so that a Guard's upward accounting walk always finds a valid
  chain, even if every user-visible handle to an intermediate ancestor
  has been dropped.
- Equivalently: a node is collectible once no part of its subtree is
  alive — no Swansong clones anywhere in the subtree, no Guards in the
  subset, no pending ShutdownCompletion on this node. This is a stronger
  retention than "no handles to this node," chosen deliberately to keep
  the accounting walk correct under every intermediate-handle-drop
  pattern. In practice the memory cost is proportional to live work
  plus live handles.
- **An ancestor does not pin a descendant.** Ancestors hold Weak
  references downward (for propagation), so dropping every user-visible
  handle and Guard in a descendant subtree allows it to be collected
  independently of the ancestor.

### 5.6 Ordering and races

- **Child creation ordered before shutdown.** If `parent.child()`
  happens-before `parent.shut_down()` (as observed by the caller), the
  resulting child must eventually observe stop (once propagation runs).
- **Shutdown ordered before child creation.** If `parent.shut_down()`
  happens-before `parent.child()`, the newly created child is created in
  the stopped state.
- **Concurrent shutdown + child creation.** No interleaving may leave a
  child un-stopped when the parent is stopped, nor leave the parent
  waiting for a child that will never be stopped. Some interleavings may
  legitimately decide whether a racing Guard is in flight or not, and that
  is fine; what is not acceptable is a permanently inconsistent tree.

### 5.7 Edge cases

- **Guard creation on a stopped Swansong.** Creating a Guard on an already-
  stopped node is allowed and behaves as a normal Guard: it contributes to
  subset counts and delays any not-yet-resolved ShutdownCompletion. It
  does not un-resolve a completion that has already resolved (futures are
  monotonic). Users racing guard-creation against shutdown get the
  outcome their ordering implies.
- **`child()` on a stopped parent.** The returned child is stopped from
  birth. Any guards subsequently registered on it behave as in the
  previous bullet. The child's `stop_event` is not required to have fired
  at birth; because the latch is observed first, any subsequently-
  registered listener will see `stopped=true` on check and not need to
  wait.
- **Guard held across child Swansong drop.** The Guard keeps the child's
  `Inner` alive (see 3.4) and continues contributing to ancestor subtree
  counts until the Guard drops. The child remains observable internally
  for the duration; externally, it is unnameable once its last Swansong
  clone is dropped.
- **Interrupt held across child Swansong drop.** Interrupt holds `Weak`.
  If the child's `Inner` has not been collected (e.g. a Guard or a
  pending ShutdownCompletion still pins it), the Interrupt observes the
  child's actual state — Running until parent propagates stop, then
  terminal. If the child's `Inner` has been collected, `Weak::upgrade`
  fails and the Interrupt returns terminal. In either case, no indefinite
  poll results.
- **Recursive depth.** Arbitrary depth is supported; no hardcoded limit.

### 5.8 Child handle-drop semantics

Dropping the last clone of a **child** (non-root) Swansong does *not*
signal shutdown. The child remains in its current state, governed by its
parent.

- If the child has outstanding Guards, the Guards pin its `Inner` and
  continue contributing to ancestor subtree counts. The child stays
  Running until the parent propagates stop (or until explicit
  `child.shut_down()` was called — but in this case there is no remaining
  handle to call it).
- If the child has no outstanding Guards or ShutdownCompletions, its
  `Inner` is collected. Any remaining Interrupts on the child observe
  termination via `Weak::upgrade` failing.

This differs from the root case (Section 3.3) because a child has a
parent to fall back to as a governing authority. The root does not, so
its handle loss implies implicit shutdown to prevent hangs.

### 5.9 Cascading shutdown propagation

When a root's implicit shutdown fires (Section 3.3), propagation proceeds
downward through the tree exactly as for an explicit `shut_down()` call.
Child subtrees receive the stop signal, their Interrupts wake, their
Guards drain on their own schedule.

## 6. Performance expectations

- **Guard create/drop** is the hot path. Expected to run per-request /
  per-task in real workloads. Should be O(1) or bounded-by-tree-depth
  with small constants, with the depth cost incurred only on genuine
  empty↔nonempty transitions. No locks.
- **Interrupt poll** is also hot. Should be a single atomic load plus
  delegation in the common case.
- **`guard_count()` query** is cold. Called for diagnostics or tests.
  O(tree size) is acceptable.
- **`shut_down()`** is cold. Called once per shutdown. O(tree size) is
  acceptable for propagation.
- **`child()`** may be called frequently (e.g. per HTTP connection).
  Should not be O(total-ever-created-children). O(live-children) or
  better. Should not contend with guard ops on unrelated nodes.
- **Tree shape** in realistic use: shallow (depth 2–4), potentially wide
  (hundreds or thousands of concurrent children of a single parent).

## 7. Out of scope

- Sibling-level coordination (no API for "wait for my siblings").
- Explicit child removal (just drop the child Swansong).
- Overlapping / non-tree subset organizations (the architecture leaves
  room; the API does not expose it).
- Quiescence or pause beyond the shutdown model.
- Reviving a stopped swansong.

## 8. Chosen implementation architecture (informative)

This section summarizes the architecture we are adopting. It is
informative: the contract is the spec above, and implementation details
may evolve. What is not informative is that the architecture must satisfy
the spec — any change must preserve Sections 3–5.

**Per-node state layout.** Each `Inner` holds:

- `stopped: AtomicBool`
- `local_guards: AtomicUsize` — Guards directly on this node.
- `nonempty_kids: AtomicUsize` — number of direct children whose subtree
  currently contains at least one Guard.
- `swansong_count: AtomicUsize` — number of live `Swansong` clones of
  this node.
- `stop_event: Event`, `zero_event: Event` — wake-ups.
- `parent: Option<Weak<Inner>>` — absent for a root, Weak for a child.
- `ancestors: Arc<[Weak<Inner>]>` — flat chain captured at `child()`
  time, empty for a root. Used by Guards to walk upward without a
  per-level lock.
- `children: Mutex<Vec<Weak<Inner>>>` — for downward propagation only.
  Touched by `shut_down()` and `child()`; never by Guard ops.

The subset's "nonempty" predicate is `local_guards > 0 ||
nonempty_kids > 0`. This is the structural fact that transitions propagate
upward.

**Guard.** A `Guard` holds `Arc<Inner>` to its home node. `Guard::new`
performs `fetch_add(1, SeqCst)` on `local_guards`; if this is the
empty→nonempty transition (prev packed value was zero), it walks
`ancestors`, `fetch_add(1, SeqCst)` on each ancestor's `nonempty_kids`,
stopping when an ancestor was already nonempty. `Guard::drop` is the
symmetric operation, with a `zero_event` notification on any ancestor
that transitions to empty while stopped.

The empty/nonempty check is made race-free by packing `local_guards`
(low half) and `nonempty_kids` (high half) into a single `AtomicU64`
and deriving the "nonempty" boolean from the entire 64-bit value
atomically. The thread that causes the packed value to cross
zero/nonzero is responsible for the upward walk; exactly one thread
wins each crossing.

**Interrupt.** `Interrupt` holds `Weak<Inner>` as today. Poll upgrades
Weak; if it fails, returns terminal. If it succeeds, check `stopped`,
return terminal if set; otherwise delegate.

**ShutdownCompletion.** Holds `Arc<Inner>`. Polls `stopped && packed
value == 0`; if not ready, registers on `zero_event` (and `stop_event`
if not yet stopped), rechecks, sleeps.

**`Swansong::shut_down()`** swaps `stopped` to true, notifies
`stop_event`, acquires the `children` mutex, walks the live children
via `Weak::upgrade`, recursively calls `stop()` on each. The mutex is a
cold-path lock; guard operations never take it.

**`Swansong::child()`** allocates a new `Inner` with
`parent: Some(Weak(self))` and `ancestors` set to `self.ancestors
++ Weak(self)`. Inserts a `Weak` into `self.children` under the mutex
(performing opportunistic eviction of dead Weaks). Seeds `stopped =
true` under the same lock if `self` is already stopped.

**`Swansong::drop`** decrements `swansong_count`; if the count reaches
zero AND `parent.is_none()` (root), calls `stop()`. For child nodes, no
action beyond the decrement.

**Cost model.**
- Guard create/drop: 1 atomic RMW in the steady state, up to O(depth)
  RMWs on genuine empty↔nonempty transitions.
- Interrupt poll: 1 Weak upgrade + 1 atomic load.
- `guard_count()`: O(subtree size) walk summing `local_guards`.
- `shut_down()`: O(subtree size) propagation.
- `child()`: O(live children of parent) for eviction + mutex acquire.

**Arc/Weak summary.**
- Swansong → Arc<Inner>
- Guard → Arc<Inner> (home node)
- ShutdownCompletion → Arc<Inner>
- Interrupt → Weak<Inner>
- parent → Weak<parent_Inner>
- ancestors → flat Weak<Inner> list
- children → Vec<Weak<child_Inner>>

No cycles. A node is kept alive by its own Swansong clones, Guards in
its subset, or pending ShutdownCompletions. It is not kept alive by
descendants or ancestors.
