#if !defined(_WIN32) && !defined(_POSIX_C_SOURCE)
#define _POSIX_C_SOURCE 199309L
#endif

#if defined(_MSC_VER) && !defined(_CRT_SECURE_NO_WARNINGS)
#define _CRT_SECURE_NO_WARNINGS
#endif

#include <stddef.h>
#include <stdint.h>
#include <setjmp.h>
#include <stdlib.h>
#include <string.h>

#ifndef RGC_MIN_HEAP
#define RGC_MIN_HEAP (1024u * 1024u)
#endif

#if defined(_MSC_VER)
#define RGC_NOINLINE __declspec(noinline)
#elif defined(__GNUC__) || defined(__clang__)
#define RGC_NOINLINE __attribute__((noinline))
#else
#define RGC_NOINLINE
#endif

typedef struct RgcHeader RgcHeader;
typedef struct RgcMarkStack RgcMarkStack;

#if defined(_MSC_VER)
/* MSVC's C11 mode omits max_align_t; double matches malloc's alignment. */
typedef double RgcMaxAlign;
#else
typedef max_align_t RgcMaxAlign;
#endif

struct RgcHeader {
    RgcHeader *next;
    size_t size;
    unsigned char marked;
    RgcMaxAlign _align;
};

struct RgcMarkStack {
    RgcHeader **items;
    size_t len;
    size_t cap;
};

static RgcHeader *rgc_objects = NULL;
static size_t rgc_live_bytes = 0;
static size_t rgc_next_collect = RGC_MIN_HEAP;
static void *rgc_stack_bottom = NULL;

static void rgc_panic(void) {
    exit(EXIT_FAILURE);
}

void rgc_init(void *stack_bottom);
void *rgc_alloc(size_t size);
void *rgc_realloc(void *ptr, size_t size);
void rgc_free(void *ptr);
RGC_NOINLINE void rgc_collect(void);

static void *rgc_payload(RgcHeader *object) {
    return (void *)(object + 1);
}

static RgcHeader *rgc_find_object(const void *ptr) {
    uintptr_t needle = (uintptr_t)ptr;

    for (RgcHeader *object = rgc_objects; object; object = object->next) {
        uintptr_t start = (uintptr_t)rgc_payload(object);
        uintptr_t end = start + object->size;
        if (needle >= start && needle < end) {
            return object;
        }
    }

    return NULL;
}

static void rgc_mark_push(RgcMarkStack *stack, const void *ptr);

static void rgc_mark_range(RgcMarkStack *stack, const void *a, const void *b) {
    uintptr_t start = (uintptr_t)a;
    uintptr_t end = (uintptr_t)b;

    if (start > end) {
        uintptr_t tmp = start;
        start = end;
        end = tmp;
    }

    size_t word = sizeof(uintptr_t);
    start = (start + word - 1u) & ~(uintptr_t)(word - 1u);
    end &= ~(uintptr_t)(word - 1u);

    for (uintptr_t cursor = start; cursor < end; cursor += word) {
        uintptr_t candidate = 0;
        memcpy(&candidate, (const void *)cursor, sizeof(candidate));
        rgc_mark_push(stack, (const void *)candidate);
    }
}

static void rgc_mark_push(RgcMarkStack *stack, const void *ptr) {
    RgcHeader *object = rgc_find_object(ptr);
    if (!object || object->marked) {
        return;
    }

    object->marked = 1;
    if (stack->len == stack->cap) {
        size_t next_cap = stack->cap ? stack->cap * 2u : 64u;
        if (next_cap < stack->cap || next_cap > SIZE_MAX / sizeof(RgcHeader *)) {
            rgc_panic();
        }
        RgcHeader **next = (RgcHeader **)realloc(stack->items, next_cap * sizeof(RgcHeader *));
        if (!next) {
            rgc_panic();
        }
        stack->items = next;
        stack->cap = next_cap;
    }
    stack->items[stack->len++] = object;
}

static void rgc_mark_roots(
    const void *a,
    const void *b,
    const void *registers,
    size_t registers_size
) {
    RgcMarkStack stack = {0};

    rgc_mark_range(&stack, registers, (const char *)registers + registers_size);
    rgc_mark_range(&stack, a, b);
    while (stack.len) {
        RgcHeader *object = stack.items[--stack.len];
        rgc_mark_range(&stack, rgc_payload(object), (char *)rgc_payload(object) + object->size);
    }

    free(stack.items);
}

static void rgc_sweep(void) {
    RgcHeader **link = &rgc_objects;
    rgc_live_bytes = 0;

    while (*link) {
        RgcHeader *object = *link;
        if (!object->marked) {
            *link = object->next;
            free(object);
            continue;
        }

        object->marked = 0;
        rgc_live_bytes += object->size;
        link = &object->next;
    }

    if (rgc_live_bytes > SIZE_MAX / 2u) {
        rgc_next_collect = SIZE_MAX;
    } else {
        size_t next = rgc_live_bytes * 2u;
        rgc_next_collect = next < RGC_MIN_HEAP ? RGC_MIN_HEAP : next;
    }
}

void rgc_init(void *stack_bottom) {
    rgc_stack_bottom = stack_bottom;
}

RGC_NOINLINE void rgc_collect(void) {
#if defined(__linux__) && defined(__x86_64__) \
    && (defined(__GNUC__) || defined(__clang__))
    uintptr_t registers[6];
    __asm__ volatile(
        "movq %%rbx, %0\n"
        "movq %%rbp, %1\n"
        "movq %%r12, %2\n"
        "movq %%r13, %3\n"
        "movq %%r14, %4\n"
        "movq %%r15, %5\n"
        : "=m"(registers[0]), "=m"(registers[1]), "=m"(registers[2]),
          "=m"(registers[3]), "=m"(registers[4]), "=m"(registers[5])
        :
        : "memory");
#elif defined(__aarch64__) && (defined(__GNUC__) || defined(__clang__))
    uintptr_t registers[11];
    __asm__ volatile(
        "mov x9, %0\n"
        "stp x19, x20, [x9, #0]\n"
        "stp x21, x22, [x9, #16]\n"
        "stp x23, x24, [x9, #32]\n"
        "stp x25, x26, [x9, #48]\n"
        "stp x27, x28, [x9, #64]\n"
        "str x29, [x9, #80]\n"
        :
        : "r"(registers)
        : "x9", "memory");
#else
    jmp_buf registers;
    (void)setjmp(registers);
#endif

    if (!rgc_stack_bottom) {
        // ponytail: embedded hosts leak safely until they provide a stack bottom.
        return;
    }

    rgc_mark_roots(&registers, rgc_stack_bottom, &registers, sizeof(registers));
    rgc_sweep();
}

void *rgc_alloc(size_t size) {
    if (size == 0u) {
        size = 1u;
    }
    if (size > SIZE_MAX - sizeof(RgcHeader)) {
        rgc_panic();
    }

    if (rgc_stack_bottom
        && (rgc_live_bytes > rgc_next_collect || size > rgc_next_collect - rgc_live_bytes)) {
        rgc_collect();
    }

    RgcHeader *object = (RgcHeader *)malloc(sizeof(RgcHeader) + size);
    if (!object) {
        rgc_collect();
        object = (RgcHeader *)malloc(sizeof(RgcHeader) + size);
        if (!object) {
            rgc_panic();
        }
    }

    object->next = rgc_objects;
    object->size = size;
    object->marked = 0;
    rgc_objects = object;
    rgc_live_bytes += size;

    return rgc_payload(object);
}

void rgc_free(void *ptr) {
    if (!ptr) {
        return;
    }

    for (RgcHeader **link = &rgc_objects; *link; link = &(*link)->next) {
        RgcHeader *object = *link;
        if (rgc_payload(object) == ptr) {
            *link = object->next;
            rgc_live_bytes -= object->size;
            free(object);
            return;
        }
    }
    // ponytail: pointers not owned by the GC are ignored; unlink walks the
    // object list, switch to a doubly linked list if frees ever dominate.
}

typedef struct {
    uint8_t level;
    size_t start;
    size_t end;
    char *message;
    size_t len;
} RiddleProcDiagnostic;

static char *riddle_proc_output = NULL;
static size_t riddle_proc_output_len = 0;
static size_t riddle_proc_call_site_start_value = 0;
static size_t riddle_proc_call_site_end_value = 0;
static RiddleProcDiagnostic *riddle_proc_diagnostics = NULL;
static size_t riddle_proc_diagnostic_len = 0;
static size_t riddle_proc_diagnostic_cap = 0;

static char *riddle_proc_copy(const uint8_t *value, size_t len) {
    if (!value && len != 0u) {
        rgc_panic();
    }
    const uint8_t *source = value ? value : (const uint8_t *)"";
    if (len == SIZE_MAX) {
        rgc_panic();
    }
    char *copy = (char *)malloc(len + 1u);
    if (!copy) {
        rgc_panic();
    }
    memcpy(copy, source, len);
    copy[len] = '\0';
    return copy;
}

void riddle_proc_begin(size_t call_site_start, size_t call_site_end) {
    free(riddle_proc_output);
    riddle_proc_output = NULL;
    riddle_proc_output_len = 0;
    riddle_proc_call_site_start_value = call_site_start;
    riddle_proc_call_site_end_value = call_site_end;
    for (size_t i = 0; i < riddle_proc_diagnostic_len; ++i) {
        free(riddle_proc_diagnostics[i].message);
    }
    riddle_proc_diagnostic_len = 0;
}

size_t riddle_proc_call_site_start(void) {
    return riddle_proc_call_site_start_value;
}

size_t riddle_proc_call_site_end(void) {
    return riddle_proc_call_site_end_value;
}

void riddle_proc_emit_diagnostic(
    uint8_t level,
    size_t start,
    size_t end,
    const uint8_t *message,
    size_t len
) {
    if (riddle_proc_diagnostic_len == riddle_proc_diagnostic_cap) {
        size_t next_cap = riddle_proc_diagnostic_cap ? riddle_proc_diagnostic_cap * 2u : 4u;
        if (next_cap < riddle_proc_diagnostic_cap
            || next_cap > SIZE_MAX / sizeof(RiddleProcDiagnostic)) {
            rgc_panic();
        }
        RiddleProcDiagnostic *next = (RiddleProcDiagnostic *)realloc(
            riddle_proc_diagnostics,
            next_cap * sizeof(RiddleProcDiagnostic)
        );
        if (!next) {
            rgc_panic();
        }
        riddle_proc_diagnostics = next;
        riddle_proc_diagnostic_cap = next_cap;
    }
    riddle_proc_diagnostics[riddle_proc_diagnostic_len].level = level;
    riddle_proc_diagnostics[riddle_proc_diagnostic_len].start = start;
    riddle_proc_diagnostics[riddle_proc_diagnostic_len].end = end;
    riddle_proc_diagnostics[riddle_proc_diagnostic_len].message = riddle_proc_copy(message, len);
    riddle_proc_diagnostics[riddle_proc_diagnostic_len].len = len;
    ++riddle_proc_diagnostic_len;
}

void riddle_proc_set_output(const uint8_t *value, size_t len) {
    free(riddle_proc_output);
    riddle_proc_output = riddle_proc_copy(value, len);
    riddle_proc_output_len = len;
}

const char *riddle_proc_output_value(void) {
    return riddle_proc_output ? riddle_proc_output : "";
}

size_t riddle_proc_output_length(void) {
    return riddle_proc_output_len;
}

size_t riddle_proc_diagnostic_count(void) {
    return riddle_proc_diagnostic_len;
}

const void *riddle_proc_diagnostics_value(void) {
    return riddle_proc_diagnostics;
}

uint8_t riddle_proc_diagnostic_level(size_t index) {
    return index < riddle_proc_diagnostic_len ? riddle_proc_diagnostics[index].level : 0u;
}

size_t riddle_proc_diagnostic_start(size_t index) {
    return index < riddle_proc_diagnostic_len
        ? riddle_proc_diagnostics[index].start
        : riddle_proc_call_site_start_value;
}

size_t riddle_proc_diagnostic_end(size_t index) {
    return index < riddle_proc_diagnostic_len
        ? riddle_proc_diagnostics[index].end
        : riddle_proc_call_site_end_value;
}

const char *riddle_proc_diagnostic_message(size_t index) {
    return index < riddle_proc_diagnostic_len ? riddle_proc_diagnostics[index].message : "";
}

size_t riddle_proc_diagnostic_message_length(size_t index) {
    return index < riddle_proc_diagnostic_len ? riddle_proc_diagnostics[index].len : 0u;
}

void *rgc_realloc(void *ptr, size_t size) {
    // rgc_alloc may trigger a collection; ptr stays reachable through this
    // frame (conservative stack scan) and stays linked until rgc_free below.
    void *next = rgc_alloc(size);

    if (ptr) {
        RgcHeader *object = (RgcHeader *)ptr - 1;
        memcpy(next, ptr, object->size < size ? object->size : size);
        rgc_free(ptr);
    }

    return next;
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

/* ---- std::time / std::random / std::fs (metadata + directory) shims ---- */
#if defined(_WIN32)
#include <windows.h>
void riddle_sleep_ms(uint64_t milliseconds) {
    Sleep((DWORD)milliseconds);
}
#else
#include <time.h>
void riddle_sleep_ms(uint64_t milliseconds) {
    struct timespec request;
    request.tv_sec = (time_t)(milliseconds / 1000u);
    request.tv_nsec = (long)((milliseconds % 1000u) * 1000000u);
    nanosleep(&request, NULL);
}
#endif

uint32_t riddle_random_u32(void) {
#if defined(_WIN32)
    static uint32_t state = 0;
    if (state == 0) {
        state = (uint32_t)GetTickCount() ^ (uint32_t)(uintptr_t)&state;
        if (state == 0) {
            state = 0x9e3779b9u;
        }
    }
    state ^= state << 13;
    state ^= state >> 17;
    state ^= state << 5;
    return state;
#else
    uint32_t value = 0;
    FILE *source = fopen("/dev/urandom", "rb");
    if (source != NULL) {
        size_t read = fread(&value, sizeof(value), 1, source);
        (void)read;
        fclose(source);
    }
    if (value == 0) {
        value = (uint32_t)time(0) ^ 0x9e3779b9u;
    }
    return value;
#endif
}

uint64_t riddle_random_u64(void) {
    uint64_t low = riddle_random_u32();
    uint64_t high = riddle_random_u32();
    return (high << 32) | low;
}

/* Keep the file-type tests available in strict POSIX feature-test modes. */
#include <sys/stat.h>
typedef struct stat RiddleStat;
#define RIDDLE_STAT(path, out) stat(path, out)
#if defined(_WIN32)
#define RIDDLE_IFDIR(mode) ((mode) & S_IFDIR)
#define RIDDLE_IFREG(mode) ((mode) & S_IFREG)
#else
#define RIDDLE_IFDIR(mode) S_ISDIR(mode)
#define RIDDLE_IFREG(mode) S_ISREG(mode)
#endif

int riddle_fs_exists(const char *path) {
    RiddleStat info;
    return RIDDLE_STAT(path, &info) == 0;
}

uint64_t riddle_fs_size(const char *path) {
    RiddleStat info;
    if (RIDDLE_STAT(path, &info) != 0) {
        return 0;
    }
    return (uint64_t)info.st_size;
}

int riddle_fs_is_file(const char *path) {
    RiddleStat info;
    if (RIDDLE_STAT(path, &info) != 0) {
        return 0;
    }
    return RIDDLE_IFREG(info.st_mode) != 0;
}

int riddle_fs_is_dir(const char *path) {
    RiddleStat info;
    if (RIDDLE_STAT(path, &info) != 0) {
        return 0;
    }
    return RIDDLE_IFDIR(info.st_mode) != 0;
}

size_t riddle_fs_read_dir(
    const char *path,
    void *names_buffer,
    void *lens_buffer,
    size_t capacity
) {
    const uint8_t **names_out = (const uint8_t **)names_buffer;
    size_t *lens_out = (size_t *)lens_buffer;
    size_t count = 0;
    size_t seen = 0;
#if defined(_WIN32)
    WIN32_FIND_DATAA entry;
    char pattern[4096];
    size_t path_len = strlen(path);
    if (path_len + 3 >= sizeof(pattern)) {
        return 0;
    }
    memcpy(pattern, path, path_len);
    pattern[path_len] = '\\'; /* backslash */
    pattern[path_len + 1] = '*';
    pattern[path_len + 2] = '\0';
    HANDLE handle = FindFirstFileA(pattern, &entry);
    if (handle == INVALID_HANDLE_VALUE) {
        return 0;
    }
    do {
        const char *name = entry.cFileName;
        if (strcmp(name, ".") == 0 || strcmp(name, "..") == 0) {
            continue;
        }
        seen += 1;
        if (count >= capacity) {
            continue;
        }
        size_t len = strlen(name);
        uint8_t *buffer = (uint8_t *)rgc_alloc(len + 1);
        memcpy(buffer, name, len + 1);
        names_out[count] = buffer;
        lens_out[count] = len;
        count += 1;
    } while (FindNextFileA(handle, &entry));
    FindClose(handle);
#else
#include <dirent.h>
    DIR *dir = opendir(path);
    if (dir == NULL) {
        return 0;
    }
    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL) {
        const char *name = entry->d_name;
        if (strcmp(name, ".") == 0 || strcmp(name, "..") == 0) {
            continue;
        }
        seen += 1;
        if (count >= capacity) {
            continue;
        }
        size_t len = strlen(name);
        uint8_t *buffer = (uint8_t *)rgc_alloc(len + 1);
        memcpy(buffer, name, len + 1);
        names_out[count] = buffer;
        lens_out[count] = len;
        count += 1;
    }
    closedir(dir);
#endif
    return seen > count ? seen : count;
}
