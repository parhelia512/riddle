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

/* Object headers live out-of-band in the registry array instead of in front
   of each payload: rgc_alloc hands out the malloc block itself (suitably
   aligned for any fundamental type per C11 7.22.3) and the collector only
   ever scans payloads it has registered. `next` is dual purpose: while
   occupied it links the address-hash bucket chain, while free it links the
   recycled-slot free list. Links store slot indices +1 encoded, so 0
   terminates a chain and slot 0 stays addressable. */
struct RgcHeader {
    uintptr_t start;
    size_t size;
    uint32_t next;
    unsigned char marked;
    unsigned char occupied;
};

struct RgcMarkStack {
    RgcHeader **items;
    size_t len;
    size_t cap;
};

/* Registry of headers for every live-or-garbage allocation. This is
   GC-external metadata kept in plain malloc/realloc memory; it is never
   scanned as a root set, so entries for garbage keep nothing alive. */
static RgcHeader *rgc_headers = NULL;
static size_t rgc_header_len = 0;
static size_t rgc_header_cap = 0;
static uint32_t rgc_free_slot = 0;
static size_t rgc_object_count = 0;

/* Address hash over occupied slots: exact-pointer membership (rgc_free,
   rgc_realloc) is O(1) average. Buckets hold +1 encoded slot indices and
   chain through RgcHeader.next. */
static uint32_t *rgc_hash = NULL;
static size_t rgc_hash_mask = 0; /* bucket count - 1 */

/* Occupied slots sorted by payload address, rebuilt once at the top of
   every collection; interior-pointer lookups during marking are then O(log
   n) binary searches. Re-sorting per collection is O(n log n), the same
   order as the mark pass itself, and it keeps registration O(1) the rest
   of the time (a permanently sorted array would need an O(n) memmove per
   allocation). */
static uint32_t *rgc_sorted = NULL;
static size_t rgc_sorted_len = 0;
static size_t rgc_sorted_cap = 0;

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

static size_t rgc_hash_of(uintptr_t start) {
    /* Multiplicative (Knuth) hashing; malloc payloads are at least 8-byte
       aligned, so the zeroed low bits are dropped before mixing. */
    return (size_t)((start >> 3u) * (uintptr_t)2654435761u) & rgc_hash_mask;
}

static void rgc_hash_grow(void) {
    size_t next_count = rgc_hash ? (rgc_hash_mask + 1u) * 2u : 64u;
    uint32_t *next;
    size_t i;

    if (next_count == 0u || next_count > SIZE_MAX / sizeof(uint32_t)) {
        rgc_panic();
    }
    next = (uint32_t *)malloc(next_count * sizeof(uint32_t));
    if (!next) {
        rgc_panic();
    }
    memset(next, 0, next_count * sizeof(uint32_t));
    free(rgc_hash);
    rgc_hash = next;
    rgc_hash_mask = next_count - 1u;

    for (i = 0; i < rgc_header_len; ++i) {
        RgcHeader *object = &rgc_headers[i];
        size_t bucket;
        if (!object->occupied) {
            continue;
        }
        bucket = rgc_hash_of(object->start);
        object->next = rgc_hash[bucket];
        rgc_hash[bucket] = (uint32_t)(i + 1u);
    }
}

static void rgc_hash_insert(uint32_t slot) {
    RgcHeader *object = &rgc_headers[slot];
    size_t buckets = rgc_hash_mask + 1u;
    size_t bucket;

    /* Grow at 75% load; the +1 accounts for the slot about to be chained.
       rgc_hash_grow rehashes occupied slots only, so `slot` is chained
       exactly once, below. */
    if (!rgc_hash || rgc_object_count + 1u > buckets - buckets / 4u) {
        rgc_hash_grow();
    }
    bucket = rgc_hash_of(object->start);
    object->next = rgc_hash[bucket];
    rgc_hash[bucket] = slot + 1u;
}

static void rgc_hash_remove(uint32_t slot) {
    RgcHeader *object = &rgc_headers[slot];
    uint32_t *link = &rgc_hash[rgc_hash_of(object->start)];

    while (*link != 0u && *link != slot + 1u) {
        link = &rgc_headers[*link - 1u].next;
    }
    if (*link != 0u) {
        *link = object->next;
    }
    object->next = 0u;
}

static RgcHeader *rgc_find_exact(const void *ptr) {
    uintptr_t needle = (uintptr_t)ptr;
    uint32_t link;

    if (!rgc_hash) {
        return NULL;
    }
    for (link = rgc_hash[rgc_hash_of(needle)]; link != 0u; link = rgc_headers[link - 1u].next) {
        RgcHeader *object = &rgc_headers[link - 1u];
        if (object->start == needle) {
            return object;
        }
    }
    return NULL;
}

static void rgc_register(uintptr_t start, size_t size) {
    uint32_t slot;
    RgcHeader *object;

    if (rgc_free_slot != 0u) {
        slot = rgc_free_slot - 1u;
        rgc_free_slot = rgc_headers[slot].next;
    } else {
        if (rgc_header_len == rgc_header_cap) {
            size_t next_cap = rgc_header_cap ? rgc_header_cap * 2u : 64u;
            RgcHeader *next;
            /* Slot links are 32-bit indices, capping the registry at
               UINT32_MAX records; more could not be addressed anyway. */
            if (next_cap < rgc_header_cap || next_cap > (size_t)UINT32_MAX) {
                rgc_panic();
            }
            next = (RgcHeader *)realloc(rgc_headers, next_cap * sizeof(RgcHeader));
            if (!next) {
                rgc_panic();
            }
            rgc_headers = next;
            rgc_header_cap = next_cap;
        }
        slot = (uint32_t)rgc_header_len;
        rgc_header_len += 1u;
    }

    object = &rgc_headers[slot];
    object->start = start;
    object->size = size;
    object->marked = 0;
    object->occupied = 0;
    object->next = 0u;
    rgc_hash_insert(slot);
    /* Occupy only after chaining, so a hash grow inside rgc_hash_insert
       cannot pick this slot up twice. */
    object->occupied = 1;
    rgc_object_count += 1u;
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

/* qsort comparator over slot indices; the base array is a file-scope
   global because the runtime, like the language, is single-threaded. */
static int rgc_sorted_cmp(const void *a, const void *b) {
    uintptr_t x = rgc_headers[*(const uint32_t *)a - 1u].start;
    uintptr_t y = rgc_headers[*(const uint32_t *)b - 1u].start;
    return x < y ? -1 : (x > y ? 1 : 0);
}

static void rgc_build_sorted(void) {
    size_t i;
    size_t len = 0;

    if (rgc_object_count > rgc_sorted_cap) {
        size_t next_cap = rgc_sorted_cap ? rgc_sorted_cap : 64u;
        uint32_t *next;
        if (rgc_object_count > SIZE_MAX / sizeof(uint32_t)) {
            rgc_panic();
        }
        while (next_cap < rgc_object_count) {
            next_cap *= 2u;
        }
        next = (uint32_t *)realloc(rgc_sorted, next_cap * sizeof(uint32_t));
        if (!next) {
            rgc_panic();
        }
        rgc_sorted = next;
        rgc_sorted_cap = next_cap;
    }

    for (i = 0; i < rgc_header_len; ++i) {
        if (rgc_headers[i].occupied) {
            rgc_sorted[len] = (uint32_t)(i + 1u);
            len += 1u;
        }
    }
    rgc_sorted_len = len;
    qsort(rgc_sorted, len, sizeof(uint32_t), rgc_sorted_cmp);
}

/* Object lookup during marking. Interior pointers (e.g. a slice into the
   middle of a buffer) must resolve to their owning object, which needs a
   predecessor search by address: a binary search over the address-sorted
   index is O(log n). Between collections the index is stale, so every
   non-marking lookup (rgc_free, rgc_realloc) uses the hash table instead. */
static RgcHeader *rgc_find_object(const void *ptr) {
    uintptr_t needle = (uintptr_t)ptr;
    size_t lo = 0;
    size_t hi = rgc_sorted_len;
    RgcHeader *object;

    while (lo < hi) {
        size_t mid = lo + (hi - lo) / 2u;
        if (rgc_headers[rgc_sorted[mid] - 1u].start <= needle) {
            lo = mid + 1u;
        } else {
            hi = mid;
        }
    }
    if (lo == 0u) {
        return NULL;
    }
    object = &rgc_headers[rgc_sorted[lo - 1u] - 1u];
    if (needle - object->start < object->size) {
        return object;
    }
    return NULL;
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
        rgc_mark_range(
            &stack,
            (const void *)object->start,
            (const void *)(object->start + object->size)
        );
    }

    free(stack.items);
}

static void rgc_sweep(void) {
    size_t i;

    rgc_live_bytes = 0;
    for (i = 0; i < rgc_header_len; ++i) {
        RgcHeader *object = &rgc_headers[i];
        if (!object->occupied) {
            continue;
        }
        if (object->marked) {
            object->marked = 0;
            rgc_live_bytes += object->size;
            continue;
        }
        rgc_hash_remove((uint32_t)i);
        free((void *)object->start);
        object->occupied = 0;
        object->next = rgc_free_slot;
        rgc_free_slot = (uint32_t)(i + 1u);
        rgc_object_count -= 1u;
    }

    /* Grow the next-collection threshold with the surviving set so
       steady-state programs do not re-collect at a fixed boundary. */
    if (rgc_live_bytes > SIZE_MAX / 2u) {
        rgc_next_collect = SIZE_MAX;
    } else {
        size_t next = rgc_live_bytes * 2u;
        rgc_next_collect = next < RGC_MIN_HEAP ? RGC_MIN_HEAP : next;
    }
}

/* ---- optional collection diagnostics ----
   Set RGC_DEBUG_STATS=1 in the environment to print one stderr line per
   collection: live bytes, live objects, and the next threshold. The flag
   is read once and off by default, so ordinary runs stay silent and
   deterministic. */
#include <stdio.h>
static int rgc_debug_stats(void) {
    static int enabled = -1;
    if (enabled < 0) {
        enabled = getenv("RGC_DEBUG_STATS") != NULL;
    }
    return enabled;
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

    rgc_build_sorted();
    rgc_mark_roots(&registers, rgc_stack_bottom, &registers, sizeof(registers));
    rgc_sweep();

    if (rgc_debug_stats()) {
        static size_t collections = 0;
        collections += 1u;
        fprintf(
            stderr,
            "rgc: collection %zu: %zu live bytes in %zu objects, next threshold %zu\n",
            collections,
            rgc_live_bytes,
            rgc_object_count,
            rgc_next_collect
        );
    }
}

void *rgc_alloc(size_t size) {
    void *block;

    if (size == 0u) {
        size = 1u;
    }

    if (rgc_stack_bottom
        && (rgc_live_bytes > rgc_next_collect || size > rgc_next_collect - rgc_live_bytes)) {
        rgc_collect();
    }

    block = malloc(size);
    if (!block) {
        rgc_collect();
        block = malloc(size);
        if (!block) {
            rgc_panic();
        }
    }

    rgc_register((uintptr_t)block, size);
    rgc_live_bytes += size;

    return block;
}

void rgc_free(void *ptr) {
    RgcHeader *object;
    uint32_t slot;

    if (!ptr) {
        return;
    }

    object = rgc_find_exact(ptr);
    if (!object) {
        return;
    }

    slot = (uint32_t)(object - rgc_headers);
    rgc_live_bytes -= object->size;
    rgc_hash_remove(slot);
    free((void *)object->start);
    object->occupied = 0;
    object->next = rgc_free_slot;
    rgc_free_slot = slot + 1u;
    rgc_object_count -= 1u;
    // ponytail: pointers not owned by the GC are ignored; membership is an
    // O(1)-average hash lookup on the exact payload address, so interior or
    // foreign pointers never free anything they should not.
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
    // frame (conservative stack scan) and stays registered until rgc_free below.
    size_t old_size = 0;
    void *next;

    if (ptr) {
        RgcHeader *object = rgc_find_exact(ptr);
        old_size = object ? object->size : 0u;
    }

    next = rgc_alloc(size);

    if (ptr) {
        if (old_size != 0u) {
            memcpy(next, ptr, old_size < size ? old_size : size);
        }
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
#include <time.h>
int64_t riddle_time(void *value) {
    return (int64_t)time((time_t *)value);
}

#if defined(_WIN32)
#include <windows.h>
void riddle_sleep_ms(uint64_t milliseconds) {
    Sleep((DWORD)milliseconds);
}
#else
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
