# Autoresearch timepass for actorpass

Discover the fastest, most memory-efficient, correct keyed monotonic scheduler
that plugs into actorpass without Tokio coupling. Research primary literature
and production implementations: hierarchical and hashed timing wheels, calendar
queues, radix heaps, pairing/binary heaps, intrusive queues, Linux timers,
Tokio, Netty, Kafka/Pulsar schedulers, generation cancellation, and newer work.
Record sources, assumptions, and failed experiments in `docs/research-log.md`.

Actorpass workloads include one current deadline per actor, rapid rescheduling
after folds, deadline cancellation on termination, large idle populations,
bursty equal deadlines, and supervisor backoff. Required semantics: never early,
at most once per generation, stale tokens cannot cancel or fire replacements,
deterministic equal-deadline order, virtual-clock testability, bounded stale
storage, and measured allocations/retained bytes. Generic ordered instants are
the floor. Tokio/std/virtual clocks are adapters. Cron strings, time zones,
durable schedules, and misfire policy are explicitly out of core scope.

State one falsifiable hypothesis per experiment. Never alter frozen tests,
benchmarks, perf harnesses, or gates. Unsafe requires written invariants, Loom
coverage of the real implementation, and Miri. Measure schedule/cancel/fire,
p50/p99 lateness overhead, memory per timer, churn, contention, and scaling—not
only one aggregate throughput score.

