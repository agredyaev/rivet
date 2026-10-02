# Rivet implementation rules

Keep the smallest explicit implementation that preserves behavior, authorization, data integrity, and resource bounds. The product contract is in [spec.md](spec.md).

1. **Data first, objects second.**
2. **Dense typed IDs over pointer identity.**
3. **Contiguous storage over heap-per-entity layouts where the workload fits.**
4. **Explicit passes over hidden object choreography.**
5. **Immutable/frozen runtime state where possible.**
6. **`Arc<Mutex<_>>` for domain state requires a concrete concurrency/FFI justification.**
7. **Temporary maps/ASTs are fine before freeze; do not retain them without need.**
8. **Do not force SoA/CSR/bitsets where the workload does not justify them.**
9. **Parse/admit once; propagate typed results.**
10. **Avoid internal serialization round-trips.**
11. **Borrow or consume before cloning.**
12. **Use request-local scratch before global caches.**
13. **One canonical state; indexes/views instead of copies.**
14. **Small local helpers before generic frameworks.**
15. **Do not merge semantically different operations to reduce LOC.**
16. **No speculative services, stores, DI, plugins, backends, or caches.**
17. **Never remove correctness boundaries as “duplication.”**
18. **Measure material optimization work.**
19. **Preserve deterministic behavior and explicit resource bounds.**
20. **Prefer measurable simplicity over abstraction.**
