/*
 * A driver package written in C against include/lemnos/package.h, for the
 * loader's tests: it shows the header is the boundary. The driver takes one
 * GPIO line, if its board entry names one, and serves a contract of two
 * methods over it.
 *
 * Built by the tests with the C compiler. A few of them build it with one of
 * the macros below defined, to make a package the loader must refuse:
 *   FAIL_REGISTRATION  register_driver fails with -12
 *   NO_DRIVER          register_driver hands over no driver
 *   NO_NAME            the driver has no name
 *   BAD_TAG            a contract tag that is not name/major
 *   BAD_NEED           a need of a kind no host knows
 */
#include <stdlib.h>
#include <string.h>

#include "lemnos/package.h"

#define STR(text) { (const uint8_t *)(text), sizeof(text) - 1 }
#define NOTHING { 0, 0 }

typedef struct Driver {
    const LemnosHostTable *host;
    LemnosHandle line;
    /* Not 0: the board entry named a line, and `line` is it. */
    uint32_t has_line;
    uint32_t base;
} Driver;

static LemnosStatus bind(const LemnosHostTable *host, void *binding, LemnosSlice settings,
                         LemnosStr link, uint32_t has_link, void **driver) {
    (void)link;
    (void)has_link;
    if (host->struct_size < sizeof(LemnosHostTable)) {
        return LEMNOS_E_VERSION_MISMATCH;
    }
    Driver *made = calloc(1, sizeof *made);
    if (made == NULL) {
        return -12;
    }
    made->host = host;
    const LemnosPackageSetting *setting = settings.ptr;
    for (size_t i = 0; i < settings.len; i++) {
        if (setting[i].key.len == 4 && memcmp(setting[i].key.ptr, "base", 4) == 0 &&
            setting[i].kind == LEMNOS_SETTING_INT) {
            made->base = (uint32_t)setting[i].int_value;
        }
    }
    LemnosStr need = STR("line");
    LemnosStr no_tag = NOTHING;
    made->has_line = host->provided(binding, need);
    if (made->has_line) {
        LemnosStatus status = host->take(binding, LEMNOS_TAKE_LINE, need, no_tag, &made->line);
        if (status != LEMNOS_OK) {
            free(made);
            return status;
        }
        /* An output, starting low. */
        LemnosPackageLineConfig config = { 1, 0, 0, 0, 0, 0, 0, 0 };
        status = host->line.configure(made->line, config);
        if (status != LEMNOS_OK) {
            host->release(made->line);
            free(made);
            return status;
        }
    }
    *driver = made;
    return LEMNOS_OK;
}

static LemnosStatus describe(void *driver, LemnosSliceMut items, uint32_t *count) {
#ifdef WITH_EXTRAS
    /* One item, with the limits and reading its extra gives. */
    if (driver != NULL && items.len >= 1) {
        LemnosPackageItem *out = (LemnosPackageItem *)items.ptr;
        LemnosPackageItem level = { 0, 0, 0, LEMNOS_ITEM_HAS_MIN | LEMNOS_ITEM_HAS_MAX, 0, 0, 1, 0,
                                    100, STR("level"), NOTHING };
        out[0] = level;
    }
    *count = driver != NULL ? 1 : 0;
#else
    (void)driver;
    (void)items;
    *count = 0;
#endif
    return LEMNOS_OK;
}

#ifdef LEMNOS_EXTRA_ITEM
#ifdef WITH_EXTRAS
/* The manifest's kind and method reading, with no driver. Items: none. */
static LemnosStatus describe_extra(void *driver, LemnosSliceMut extras, uint32_t *count) {
    LemnosPackageExtra *out = (LemnosPackageExtra *)extras.ptr;
    if (driver != NULL) {
        if (extras.len >= 1) {
            LemnosPackageExtra item = { .what = LEMNOS_EXTRA_ITEM, .reading = 1,
                                        .flags = LEMNOS_EXTRA_HAS_READING | LEMNOS_EXTRA_HAS_STEP |
                                                 LEMNOS_EXTRA_HAS_WARN_LOW |
                                                 LEMNOS_EXTRA_HAS_WARN_HIGH |
                                                 LEMNOS_EXTRA_HAS_GROUP |
                                                 LEMNOS_EXTRA_HAS_UNIT_STATE |
                                                 LEMNOS_EXTRA_UNIT_DIMENSIONLESS,
                                        .index = 0, .step = 5, .warn_low = 10, .warn_high = 90,
                                        .text = STR("grp") };
            out[0] = item;
        }
        *count = 1;
        return LEMNOS_OK;
    }
    if (extras.len >= 2) {
        LemnosPackageExtra kind = { .what = LEMNOS_EXTRA_CONTRACT, .flags = LEMNOS_EXTRA_HAS_GROUP,
                                    .text = STR("sensor") };
        LemnosPackageExtra reading = { .what = LEMNOS_EXTRA_METHOD, .reading = 2,
                                       .flags = LEMNOS_EXTRA_HAS_READING, .method = 1 };
        out[0] = kind;
        out[1] = reading;
    }
    *count = 2;
    return LEMNOS_OK;
}
#define DESCRIBE_EXTRA describe_extra
#else
#define DESCRIBE_EXTRA NULL
#endif
#endif

static LemnosStatus health(void *driver, LemnosPackageHealth *health) {
    (void)driver;
    LemnosPackageHealth ok = { LEMNOS_HEALTH_OK, 0, NOTHING };
    *health = ok;
    return LEMNOS_OK;
}

static LemnosStatus dispatch(void *driver, uint8_t contract, uint16_t method, LemnosSlice args,
                             LemnosSliceMut reply, size_t *len) {
    Driver *made = driver;
    *len = 0;
    if (contract != 0) {
        return -22;
    }
    if (method == 1) {
        /* level() -> u32: the line's value (0 with no line), plus the `base` setting. */
        uint32_t value = 0;
        if (made->has_line) {
            LemnosStatus status = made->host->line.get(made->line, &value);
            if (status != LEMNOS_OK) {
                return status;
            }
        }
        if (reply.len < 4) {
            return LEMNOS_E_BUFFER_TOO_SMALL;
        }
        uint32_t level = made->base + value;
        uint8_t *out = reply.ptr;
        for (int i = 0; i < 4; i++) {
            out[i] = (uint8_t)(level >> (8 * i));
        }
        *len = 4;
        return LEMNOS_OK;
    }
    if (method == 2) {
        /* set(value: bool) */
        if (args.len < 1) {
            return -22;
        }
        if (!made->has_line) {
            return -19;
        }
        return made->host->line.set(made->line, ((const uint8_t *)args.ptr)[0]);
    }
    return -95;
}

static LemnosStatus tick(void *driver, uint64_t now_ns, uint64_t *next_ns) {
    (void)driver;
    (void)now_ns;
    *next_ns = LEMNOS_NO_TICK;
    return LEMNOS_OK;
}

static LemnosStatus caller(void *driver, uint64_t who) {
    (void)driver;
    (void)who;
    return LEMNOS_OK;
}

static LemnosStatus drop_driver(void *driver) {
    Driver *made = driver;
    if (made != NULL) {
        if (made->has_line) {
            made->host->release(made->line);
        }
        free(made);
    }
    return LEMNOS_OK;
}

#ifdef BAD_NEED
#define NEED_KIND 99
#else
#define NEED_KIND LEMNOS_TAKE_LINE
#endif
static const LemnosPackageNeed NEEDS[] = {
    { STR("line"), NEED_KIND, 1, NOTHING },
};

#define PLAIN(kind) { (kind), 0, NOTHING, NOTHING }
static const LemnosPackageField SET_PARAMS[] = {
    { STR("value"), PLAIN(1) },
};
static const LemnosPackageMethod METHODS[] = {
    { 1, 0, 0, STR("level"), NOTHING, PLAIN(4) },
    { 2, 0, 0, STR("set"), { SET_PARAMS, 1 }, PLAIN(0) },
};

#ifdef BAD_TAG
#define TAG STR("example.c")
#else
#define TAG STR("example.c/1")
#endif
static const LemnosPackageContract CONTRACTS[] = {
    { TAG, { METHODS, 2 } },
};

#ifdef NO_NAME
#define NAME NOTHING
#else
#define NAME STR("c-package")
#endif
static const LemnosPackageDriver DRIVER = {
    { NAME, { NEEDS, 1 }, { CONTRACTS, 1 } },
    { sizeof(LemnosDriverTable), 0, bind, describe, health, dispatch, tick, caller, caller,
      drop_driver, NULL, NULL
#ifdef LEMNOS_EXTRA_ITEM
      , DESCRIBE_EXTRA
#endif
    },
};

static LemnosStatus register_driver(LemnosSlice *driver) {
    driver->ptr = &DRIVER;
#ifdef NO_DRIVER
    driver->len = 0;
#else
    driver->len = 1;
#endif
#ifdef FAIL_REGISTRATION
    /* What it wrote counts for nothing once it says it failed. */
    return -12;
#else
    return LEMNOS_OK;
#endif
}

#ifdef FUTURE_LAYOUT
/* A boundary this host does not know: its layout hash is none of the host's. */
#define VERSION_HERE { sizeof(LemnosAbiVersion), LEMNOS_PACKAGE_MAJOR, LEMNOS_PACKAGE_MINOR, 0x1234 }
#else
#define VERSION_HERE LEMNOS_PACKAGE_VERSION_INIT
#endif

const LemnosPackageDescriptor lemnos_package = {
    LEMNOS_PACKAGE_MAGIC,
    VERSION_HERE,
    register_driver,
};
