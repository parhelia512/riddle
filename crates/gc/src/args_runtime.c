#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#if defined(_WIN32)

#define WIN32_LEAN_AND_MEAN
#include <windows.h>

static void riddle_args_fail(void) {
    abort();
}

typedef struct {
    uint8_t *bytes;
    size_t len;
} RiddleProcessArg;

typedef struct {
    uint16_t *items;
    size_t len;
    size_t cap;
} RiddleWideBuffer;

typedef struct {
    RiddleProcessArg *items;
    size_t len;
    size_t cap;
} RiddleArgBuffer;

static INIT_ONCE riddle_args_once = INIT_ONCE_STATIC_INIT;
static int32_t riddle_process_argc = 0;
static RiddleProcessArg *riddle_process_argv = NULL;

static void *riddle_args_realloc(void *pointer, size_t count, size_t item_size) {
    if (item_size != 0u && count > SIZE_MAX / item_size) {
        riddle_args_fail();
    }
    void *next = realloc(pointer, count * item_size);
    if (!next) {
        riddle_args_fail();
    }
    return next;
}

static void riddle_wide_push(RiddleWideBuffer *buffer, uint16_t value) {
    if (buffer->len == buffer->cap) {
        size_t next_cap = buffer->cap ? buffer->cap * 2u : 16u;
        if (next_cap < buffer->cap) {
            riddle_args_fail();
        }
        buffer->items = (uint16_t *)riddle_args_realloc(
            buffer->items,
            next_cap,
            sizeof(uint16_t)
        );
        buffer->cap = next_cap;
    }
    buffer->items[buffer->len++] = value;
}

static size_t riddle_wtf8_len(const uint16_t *units, size_t len) {
    size_t output_len = 0u;
    for (size_t index = 0u; index < len; ++index) {
        uint32_t value = units[index];
        if (value >= 0xd800u && value <= 0xdbffu && index + 1u < len) {
            uint32_t next = units[index + 1u];
            if (next >= 0xdc00u && next <= 0xdfffu) {
                value = 0x10000u + ((value - 0xd800u) << 10u) + (next - 0xdc00u);
                ++index;
            }
        }
        size_t encoded = value <= 0x7fu ? 1u : value <= 0x7ffu ? 2u : value <= 0xffffu ? 3u : 4u;
        if (output_len > SIZE_MAX - encoded) {
            riddle_args_fail();
        }
        output_len += encoded;
    }
    return output_len;
}

static RiddleProcessArg riddle_wtf8_encode(const uint16_t *units, size_t len) {
    size_t output_len = riddle_wtf8_len(units, len);
    if (output_len == SIZE_MAX) {
        riddle_args_fail();
    }
    uint8_t *output = (uint8_t *)malloc(output_len + 1u);
    if (!output) {
        riddle_args_fail();
    }

    size_t offset = 0u;
    for (size_t index = 0u; index < len; ++index) {
        uint32_t value = units[index];
        if (value >= 0xd800u && value <= 0xdbffu && index + 1u < len) {
            uint32_t next = units[index + 1u];
            if (next >= 0xdc00u && next <= 0xdfffu) {
                value = 0x10000u + ((value - 0xd800u) << 10u) + (next - 0xdc00u);
                ++index;
            }
        }
        if (value <= 0x7fu) {
            output[offset++] = (uint8_t)value;
        } else if (value <= 0x7ffu) {
            output[offset++] = (uint8_t)(0xc0u | (value >> 6u));
            output[offset++] = (uint8_t)(0x80u | (value & 0x3fu));
        } else if (value <= 0xffffu) {
            output[offset++] = (uint8_t)(0xe0u | (value >> 12u));
            output[offset++] = (uint8_t)(0x80u | ((value >> 6u) & 0x3fu));
            output[offset++] = (uint8_t)(0x80u | (value & 0x3fu));
        } else {
            output[offset++] = (uint8_t)(0xf0u | (value >> 18u));
            output[offset++] = (uint8_t)(0x80u | ((value >> 12u) & 0x3fu));
            output[offset++] = (uint8_t)(0x80u | ((value >> 6u) & 0x3fu));
            output[offset++] = (uint8_t)(0x80u | (value & 0x3fu));
        }
    }
    output[output_len] = 0u;
    RiddleProcessArg argument = { output, output_len };
    return argument;
}

static void riddle_args_push(RiddleArgBuffer *buffer, const uint16_t *units, size_t len) {
    if (buffer->len == buffer->cap) {
        size_t next_cap = buffer->cap ? buffer->cap * 2u : 4u;
        if (next_cap < buffer->cap) {
            riddle_args_fail();
        }
        buffer->items = (RiddleProcessArg *)riddle_args_realloc(
            buffer->items,
            next_cap,
            sizeof(RiddleProcessArg)
        );
        buffer->cap = next_cap;
    }
    buffer->items[buffer->len++] = riddle_wtf8_encode(units, len);
}

static RiddleWideBuffer riddle_current_exe(void) {
    RiddleWideBuffer output = { NULL, 0u, 0u };
    size_t cap = 260u;
    for (;;) {
        if (cap > UINT32_MAX) {
            return output;
        }
        output.items = (uint16_t *)riddle_args_realloc(output.items, cap, sizeof(uint16_t));
        DWORD len = GetModuleFileNameW(NULL, (WCHAR *)output.items, (DWORD)cap);
        if (len == 0u) {
            free(output.items);
            output.items = NULL;
            return output;
        }
        if ((size_t)len < cap) {
            output.len = (size_t)len;
            output.cap = cap;
            return output;
        }
        if (cap > SIZE_MAX / 2u) {
            riddle_args_fail();
        }
        cap *= 2u;
    }
}

static RiddleArgBuffer riddle_parse_windows_args(const uint16_t *command_line) {
    RiddleArgBuffer output = { NULL, 0u, 0u };
    if (!command_line || command_line[0] == 0u) {
        RiddleWideBuffer exe = riddle_current_exe();
        riddle_args_push(&output, exe.items, exe.len);
        free(exe.items);
        return output;
    }

    const uint16_t *cursor = command_line;
    RiddleWideBuffer current = { NULL, 0u, 0u };
    int in_quotes = 0;
    while (*cursor != 0u) {
        uint16_t value = *cursor++;
        if (value == (uint16_t)'"') {
            in_quotes = !in_quotes;
        } else if (!in_quotes && (value == (uint16_t)' ' || value == (uint16_t)'\t')) {
            break;
        } else {
            riddle_wide_push(&current, value);
        }
    }
    while (*cursor == (uint16_t)' ' || *cursor == (uint16_t)'\t') {
        ++cursor;
    }
    riddle_args_push(&output, current.items, current.len);
    current.len = 0u;

    in_quotes = 0;
    while (*cursor != 0u) {
        uint16_t value = *cursor++;
        if (!in_quotes && (value == (uint16_t)' ' || value == (uint16_t)'\t')) {
            riddle_args_push(&output, current.items, current.len);
            current.len = 0u;
            while (*cursor == (uint16_t)' ' || *cursor == (uint16_t)'\t') {
                ++cursor;
            }
        } else if (value == (uint16_t)'\\') {
            size_t backslashes = 1u;
            while (*cursor == (uint16_t)'\\') {
                ++backslashes;
                ++cursor;
            }
            if (*cursor == (uint16_t)'"') {
                for (size_t index = 0u; index < backslashes / 2u; ++index) {
                    riddle_wide_push(&current, (uint16_t)'\\');
                }
                if (backslashes % 2u == 1u) {
                    ++cursor;
                    riddle_wide_push(&current, (uint16_t)'"');
                }
            } else {
                for (size_t index = 0u; index < backslashes; ++index) {
                    riddle_wide_push(&current, (uint16_t)'\\');
                }
            }
        } else if (value == (uint16_t)'"' && in_quotes) {
            if (*cursor == (uint16_t)'"') {
                ++cursor;
                riddle_wide_push(&current, (uint16_t)'"');
            } else if (*cursor != 0u) {
                in_quotes = 0;
            } else {
                break;
            }
        } else if (value == (uint16_t)'"') {
            in_quotes = 1;
        } else {
            riddle_wide_push(&current, value);
        }
    }
    if (current.len != 0u || in_quotes) {
        riddle_args_push(&output, current.items, current.len);
    }
    free(current.items);
    return output;
}

static BOOL CALLBACK riddle_args_initialize_once(
    PINIT_ONCE once,
    PVOID parameter,
    PVOID *context
) {
    (void)once;
    (void)parameter;
    (void)context;
    RiddleArgBuffer parsed = riddle_parse_windows_args((const uint16_t *)GetCommandLineW());
    if (parsed.len > INT32_MAX) {
        riddle_args_fail();
    }
    riddle_process_argc = (int32_t)parsed.len;
    riddle_process_argv = parsed.items;
    return TRUE;
}

static void riddle_args_ensure_initialized(void) {
    if (!InitOnceExecuteOnce(&riddle_args_once, riddle_args_initialize_once, NULL, NULL)) {
        riddle_args_fail();
    }
}

void riddle_args_init(int32_t argc, char **argv) {
    (void)argc;
    (void)argv;
    riddle_args_ensure_initialized();
}

int32_t riddle_argc(void) {
    riddle_args_ensure_initialized();
    return riddle_process_argc;
}

void *riddle_argv_at(int32_t index) {
    riddle_args_ensure_initialized();
    if (index < 0 || index >= riddle_process_argc) {
        return NULL;
    }
    return (void *)riddle_process_argv[index].bytes;
}

size_t riddle_argv_len(int32_t index) {
    riddle_args_ensure_initialized();
    if (index < 0 || index >= riddle_process_argc) {
        return 0u;
    }
    return riddle_process_argv[index].len;
}

#else

static int32_t riddle_process_argc = 0;
static char **riddle_process_argv = NULL;

void riddle_args_init(int32_t argc, char **argv) {
    int32_t count = 0;
    while (argv && count < argc && argv[count]) {
        ++count;
    }
    riddle_process_argc = count;
    riddle_process_argv = argv;
}

int32_t riddle_argc(void) {
    return riddle_process_argc;
}

void *riddle_argv_at(int32_t index) {
    if (!riddle_process_argv || index < 0 || index >= riddle_process_argc) {
        return NULL;
    }
    return (void *)riddle_process_argv[index];
}

size_t riddle_argv_len(int32_t index) {
    const char *value = (const char *)riddle_argv_at(index);
    return value ? strlen(value) : 0u;
}

#endif
