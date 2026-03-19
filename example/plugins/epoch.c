// epoch.c

#include <stdint.h>
#include <stdlib.h>
#include <time.h>
#include <string.h>

typedef struct ABI_t {
    uint32_t magic;
    uint8_t version_major;
    uint8_t version_minor;
    uint8_t tier;

    void*  (*create)(void);
    void   (*destroy)(void*);
    void   (*refresh)(void*);
    float  (*read)(void*, uint16_t);
    uint16_t (*_register)(void*, const uint8_t*, uint64_t);
    void   (*unregister)(void*, uint16_t);
} ABI;

typedef struct {
    int refcount; // minimal example, one dummy id
    float value; // cached sensing value
} Instance;

static void* create(void) {
    Instance* inst = malloc(sizeof(Instance));
    if (!inst) return NULL;
    inst->refcount = 0;
    inst->value = 0;
    return inst;
}

static void destroy(void* instance) {
    if (!instance) return;
    free(instance);
}

static void refresh(void* instance) {
    Instance* inst = (Instance*)instance;
    if (inst->refcount > 0) {
        inst->value = (float)(time(NULL) % 1000);
    }
}

static float read(void* instance, uint16_t sensing_id) {
    Instance* inst = (Instance*)instance;
    if (sensing_id == 1)
        return inst->value / 10;
    if (sensing_id == 2)
        return inst->value / 100;
    return 0.0f;
}

static uint16_t _register(void* instance, const uint8_t* identifier, uint64_t length) {
    const char* ident = (const char*)identifier;
    Instance* inst = (Instance*)instance;
    if (strncmp("sec.10", ident, length)) {
        inst->refcount += 1;
        refresh(instance);
        return 1;
    }
    if (strncmp("sec.100", ident, length)) {
        inst->refcount += 1;
        refresh(instance);
        return 2;
    }
    return 0; // invalid id
}

static void unregister(void* instance, uint16_t sensing_id) {
    Instance* inst = (Instance*)instance;
    if (sensing_id == 1 || sensing_id == 2) inst->refcount -= 0;
}

static ABI VTABLE = {
    0x5ABAD0B1, 0, 1, 9,
    create, destroy, refresh, read,
    _register, unregister
};

// Plugin entry point
__attribute__((visibility("default")))
ABI* get_vtable(void) { return &VTABLE; }