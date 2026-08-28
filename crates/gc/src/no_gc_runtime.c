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

/* ---- std::fs shim (avoids clashing with <stdio.h> prototypes) ---- */
#include <stdio.h>
#include <stdint.h>
size_t riddle_fs_fopen(const char *path, const char *mode) {
    return (size_t)(uintptr_t)fopen(path, mode);
}
int riddle_fs_fclose(size_t stream) {
    return fclose((FILE *)(uintptr_t)stream);
}
size_t riddle_fs_fread(void *buffer, size_t size, size_t count, size_t stream) {
    return fread(buffer, size, count, (FILE *)(uintptr_t)stream);
}
size_t riddle_fs_fwrite(void *buffer, size_t size, size_t count, size_t stream) {
    return fwrite(buffer, size, count, (FILE *)(uintptr_t)stream);
}
int riddle_fs_fflush(size_t stream) {
    return fflush((FILE *)(uintptr_t)stream);
}
