// epoch.c

#include <stdint.h>
#include <string.h>
#include <stdlib.h>

typedef struct ABI_t {
    uint32_t magic;
    uint8_t version_major;
    uint8_t version_minor;
    uint8_t tier;

    // Creates internal data buffer for a instance and assign the pointer for it.
    // This data buffer *should* contain all the sensing values for every keys registered.
    uint32_t (*create)(void** instance);
    // Destroy internal data (returned from 'create') for a instance.
    uint32_t (*destroy)(void* instance);
    // Collect data, refresh the sensing values as needed.
    // This method will be called periodically as specified in 'tier'.
    // Set 0 for boolean value to indicate false, non zero otherwise.
    uint32_t (*refresh)(void* instance);
    // Given an identifier string , return the sensing id.
    // The idenfifier string is gven in utf8 encoding, with plugin name prefix stripped.
    // If the plugin can provide sensing value for requested identifier it should set valid sensing id ot 'out'.
    // Sensing id structure:
    //     16            15                 8                 0
    //      | type (1 bit)| custom (7 bits) | offset (8 bits) |
    //     type: 0 for float, 1 for boolean
    //     custom: plugins are free to put any info here.
    //     offset: 64 bit stride offset of the sensing value from the start of the instance pointer.
    // The following routine (or equivalent) will be used When the system reads the sensing value
    //     val = ((double*) instance)[sid & 0xFF]
    //     (out & 0x8000) ? (val == 0.0) : (val)
    // On error, put a 1-based index of errorneous identifier field (sperated by '.') to 'out',
    // or 0 (untouched default) to refer to the whole identifier path.
    uint32_t (*_register)(void* instance, const uint8_t* ident, uint64_t ident_len, uint16_t *out);
    // This will be called when one of the sprites that uses sensing_id unloads.
    // If you don't manager reference count of sensing metrics, you can ignore this call.
    uint32_t (*unregister)(void* instance, uint16_t sensing_id);
    // All the methods should return an nonzero-error code to indicate error. (0 on success)
    // This method should provide a single line message (without newline) describing the error state.
    // The message string must persist until next time 'message' is called for this instance.
    // 'instance' may be NULL if the plugin failed during 'create'.
    uint32_t (*message)(void* instance, uint32_t errcode, const uint8_t** msg, uint64_t* msg_len);
} ABI;

const double REFRESH_HZ = 2;

typedef struct {
    double bysec;
    double by10sec;
    double over5sec;
    int refcount;
    unsigned count;
} Instance;

enum Error {
    Success,
    CannotAlloc,
    UnknownTerm,
    UnknownNum,
    UnknownErrcode,
};

static uint32_t message(void* instance, uint32_t errcode, const uint8_t** msg, uint64_t* msg_len) {
    char* mesg = NULL;
    uint32_t ret = Success;
    switch (errcode) {
        case Success:
            mesg = "success";
            break;
        case CannotAlloc:
            mesg = "cannot allocate instance buffer";
            break;
        case UnknownTerm:
            mesg = "unknown first term";
            break;
        case UnknownNum:
            mesg = "unsupported number";
            break;
        default:
            mesg = "unknown error code";
            ret = UnknownErrcode;
    }

    *msg = mesg;
    *msg_len = strlen(mesg);
    return ret;
}

static uint32_t create(void** instance) {
    Instance* inst = malloc(sizeof(Instance));
    if (!inst) return CannotAlloc;
    memset(inst, 0, sizeof(Instance));
    *instance = inst;
    return Success;
}

static uint32_t destroy(void* instance) {
    if (instance) {
        free(instance);
    }
    return Success;
}

static uint32_t refresh(void* instance) {
    Instance* inst = (Instance*)instance;
    if (inst->refcount > 0) {
        inst->count += 1;
        inst->bysec = inst->count / REFRESH_HZ;
        inst->by10sec = inst->count / REFRESH_HZ / 10.0;
        inst->over5sec = inst->bysec > 5? 1.0: 0.0;
    }
    return Success;
}

static uint32_t _register(void* instance, const uint8_t* identifier, uint64_t ident_len, uint16_t *out) {
    const char* ident = (const char*)identifier;
    Instance* inst = (Instance*)instance;

    uint16_t sid;
    if (ident_len == 6 && 0 == strncmp("over5s", ident, 6)) {
        sid = 2;
    } else {
        if (0 != strncmp("sec.", ident, 4)) {
            *out = 1;
            return UnknownTerm;
        }

        if (ident_len == 5 && 0 == strncmp("1", ident+4, 1)) {
            sid = 0;
        } else if (ident_len == 6 && 0 == strncmp("10", ident+4, 2)) {
            sid = 1;
        } else {
            *out = 2;
            return UnknownNum;
        }
    }

    inst->refcount += 1;
    *out = sid;

    return refresh(instance);
}

static uint32_t unregister(void* instance, uint16_t sensing_id) {
    Instance* inst = (Instance*)instance;
    if (sensing_id == 0 || sensing_id == 1 || sensing_id == 2) {
        inst->refcount -= 1;
    }
    return Success;
}

static ABI VTABLE = {
    0x5ABAD0B1, 0, 0, 9,
    create, destroy, refresh,
    _register, unregister, message,
};

// Plugin entry point
__attribute__((visibility("default")))
ABI* get_vtable(void) { return &VTABLE; }