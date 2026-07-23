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

struct RgcHeader {
    RgcHeader *next;
    size_t size;
    unsigned char marked;
    max_align_t _align;
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

void rgc_init(void *stack_bottom);
void *rgc_alloc(size_t size);
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
            abort();
        }
        RgcHeader **next = (RgcHeader **)realloc(stack->items, next_cap * sizeof(RgcHeader *));
        if (!next) {
            abort();
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
#if defined(__aarch64__) && (defined(__GNUC__) || defined(__clang__))
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

#if defined(__aarch64__) && (defined(__GNUC__) || defined(__clang__))
    rgc_mark_roots(registers, rgc_stack_bottom, registers, sizeof(registers));
#else
    rgc_mark_roots(&registers, rgc_stack_bottom, &registers, sizeof(registers));
#endif
    rgc_sweep();
}

void *rgc_alloc(size_t size) {
    if (size == 0u) {
        size = 1u;
    }
    if (size > SIZE_MAX - sizeof(RgcHeader)) {
        abort();
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
            abort();
        }
    }

    object->next = rgc_objects;
    object->size = size;
    object->marked = 0;
    rgc_objects = object;
    rgc_live_bytes += size;

    return rgc_payload(object);
}
