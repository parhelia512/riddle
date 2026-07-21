# Riddle default GC runtime

This crate owns Riddle's default non-moving, conservative mark-sweep runtime.
The compiler does not embed this implementation; `clue` selects it when a
binary package does not provide a custom runtime source.

Every runtime provider implements this C ABI:

```c
void rgc_init(void *stack_bottom);
void *rgc_alloc(size_t size);
void rgc_collect(void);
```

`rgc_alloc` must return a non-null, suitably aligned address that does not move
while references may still exist. An allocator without collection may ignore
`stack_bottom` and implement `rgc_collect` as a no-op. The current ABI does not
support moving collection, per-object freeing, finalizers, or thread stack
registration.
