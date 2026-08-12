# Riddle default GC runtime

This crate owns Riddle's default non-moving, conservative mark-sweep runtime.
The compiler does not embed this implementation; `clue` selects it when a
binary package does not provide a custom runtime source.

Every runtime provider implements this C ABI:

```c
void rgc_init(void *stack_bottom);
void *rgc_alloc(size_t size);
void *rgc_realloc(void *ptr, size_t size);
void rgc_free(void *ptr);
void rgc_collect(void);
```

Clue links the platform process-argument runtime separately, so custom memory
runtime providers do not implement `std::env` argument functions.

`rgc_alloc` must return a non-null, suitably aligned address that does not move
while references may still exist. An allocator without collection may ignore
`stack_bottom` and implement `rgc_collect` as a no-op. `rgc_realloc` must
preserve the existing prefix and may return a different address; `rgc_free`
must accept null and release an allocation owned by the provider. The current
ABI does not support moving collection, finalizers, or thread stack
registration.
