#include <stddef.h>
#include <stdlib.h>

static void riddle_memory_fail(void) {
    abort();
}

void *riddle_alloc(size_t size) {
    void *pointer = malloc(size ? size : 1u);
    if (!pointer) {
        riddle_memory_fail();
    }
    return pointer;
}

void *riddle_realloc(void *pointer, size_t size) {
    void *next = realloc(pointer, size ? size : 1u);
    if (!next) {
        riddle_memory_fail();
    }
    return next;
}

void riddle_free(void *pointer) {
    free(pointer);
}
