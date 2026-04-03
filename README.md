# Mythic Monitor

![Dynamic TOML Badge](https://img.shields.io/badge/dynamic/toml?url=https%3A%2F%2Fraw.githubusercontent.com%2Frefqiez%2Fmythic-monitor%2Frefs%2Fheads%2Fmaster%2Fapp%2FCargo.toml&query=%24.package.version&label=version)
![GitHub License](https://img.shields.io/github/license/refqiez/mythic-monitor)


A lightweight sprite widget manager for visual system monitoring.


# Overview

[[screenshot.png]] TODO

Mythic Monitor (or mythic) is a lightweight desktop widget manager that displays animated sprites reacting to system activity. Instead of graphs or dashboards, system metrics can be represented through sprites.

Widgets appear as always-on-top, non-interactable, click-through overlays. Their appearance and behavior fully configurable via config files. No GUI. Absolute minimal UI.

Mythic Monitor is designed to hold minimal resources even with several widgets running. Staying lightweight, as system all monitors should.


<!--
TODO
# Installation

## Requirements

## Download

## Build from Source

## Verify Installation

# Quick Start
-->

# Configuration

All configuration is done via editing files in the filesystem.
See the `examples` directory for working samples.

## Terminology

**widget**: A runtime entity displayed on the desktop. Widgets appear as always-on-top, click-through overlays that render animated sprites reacting to system activity. Conceptually widgets are _sprite definition_ + _window render_.


**sprite**: A configuration and asset definition used to create a widget. Sprites are state machines working on system metrics to decide which clip to display.

A sprite definition specifies:
- the states of the widget
- the clips used in each state
- the transitions between states
- other behavior and rendering properties

**state**: represents a visual condition of a widget. Each sprite defines one or more states, and the widget is always in exactly one state while running.

A state definition specifies:
- clips from which to choose randomly
- transition condition to other states

**clip**: A `.webp` animation rendered while a widget is in a particular state.

Following properties of '.webp' are respected
- frame pixel data
- transparency
- delays for each frames
- loop count

**sensing**: refers to obtaining values from external sources that widgets can react to. Most commonly this means system metrics such as CPU usage, memory usage, disk activity, or network throughput.

**TCE**: is short for Transition Condition Expression. TCEs are used to determine if a transition is allowed in sprite state machine. They are string values with `expression` syntax (see [expr.md](app/src/parser/expr.md) for details).

## Path

Most the configurations are read from the app's root directory.
| Platform  | Value         | Example |
|-----------|---------------|---------|
| Linux     | `$XDG_DATA_HOME/mythic` or `$HOME/.local/share/mythic` | `/home/alice/.local/share/mythic` |
| Windows   | `%APPDATA%\mythic` | `C:\Users\Alice\AppData\Roaming\mythic` |
<!-- | macOS     | `$HOME/Library/Application Support/mythic` | `/Users/Alice/Library/Application Support/mythic` | -->

If `app_root` does not exist, it will create one with all the subdirectories.

`app_root` can be overridden by command line argument.

The directory structure is as follows
```
[app_root]/
├── sprites/             # widget definitions, clips
│   ├── list.toml        # list the sprites to load
│   ├── [sprite].toml    # sprite definition
│   ├── [clip].webp      # clip file
│   └── [sprite]         # nested sprites also possible
│       ├── [name].toml
│       └── [clip].webp
├── plugins/             # plugins for extended sensing
│   ├── gpu.dll          # windows shared library format
│   └── linux            # sprite.toml
│       └── gpu.so       # linux shared library format
└── misc/
    ├── config.toml      # non-widget configurations
    ├── running          # indication of process running, delete this to shut it down
    ├── now.log          # current log file
    └── [epochtime].log  # old log files
```

The program watches `sprites` and `plugins` directories recursively. Any changes made in those directories applies immediately.

## Format

Most configuration files use a simplified subset of TOML. This custom format intentionally restricts some TOML features in order to keep the parser small and fast.

The syntax remains largely compatible with standard TOML, but several features are not supported or behave differently. For the complete specification, see [toml.md](app/src/parser/toml.md).

Notable differences are:
- Key-value must contained in a line. (especially for string, table, array values)
- No date/time, No numeric separators. No hex/octal/binary.
- No nested table access via dotted keys.
- Allows duplicated keys.

If not state otherwise, the last assigned values are prefered for duplicated keys.

### version

Fields that appear before any section in a TOML file are considered _global fields_.  All other fields belong to sections.

Every TOML files are required to have `version` global field with value:

- A string following _SemVer_ format: `MAJOR.MINOR` (minor can be omitted).
- Prefixes control version matching:
  - `=` → exact match required
  - `^` or no prefix → allows updates to later minor versions
- Minor version may be omitted as in `1.`. it will match any minor version.

```toml
version = "1.2"   # allows 1.2, 1.3, 1.4, etc.
version = "^1.2"  # same as above
version = "=1.2"  # only exact 1.2 allowed
version = "1."    # any minor version of 1.x allowed
version = "=1."   # same as above
```

### `sprites/list.toml`

The `sprite/list.toml` file defines all widgets to be loaded.
Each section in this file represents a single widget, with the section name serving as the widget's runtime name. (currently wdget name has no use)

Fields:
- **`sprite`**: A string specifying the path to the sprite definition. Paths are relative to the directory of `list.toml`.
    + If the path refers to a `.toml` file, that file is loaded as the sprite definition.
    + If the path refers to a directory containing exactly one `.toml` file, that file is loaded.
    + Otherwise, an error occurs.

- **`size.width`, `size.height`**: Widget dimensions in pixels.
    + If **both** are specified → clips are rescaled to match both dimensions.
    + If **only one** is specified → clips are rescaled, maintaining original aspect ratio.
    + If **neither** is specified → clips are rendered at their original size.

- **`pos.*`**: Widget screen position. Values are in pixels relative to the top-left of the screen.
    + vertical: `top`, `ycenter`, `bottom`
    + Horizontal: `left`, `xcenter`, `right`

- **`param.*`**: Parameter to be sent to the sprite that will be used in TCEs. Must be string value. See [sprites.toml](#spritesspritetoml)

```toml
[sonic1]
sprite = "sonic"
param.sensor = "cpu"
size.width = 500
size.height = 100
pos.bottom = 150
pos.xcenter = 100
```

### `sprites/**/[sprite].toml`

Sprite definition files describe widgets' states and behavior. The file can have any name with a `.toml` extension and can be nested as deeply as you want.

Each section defines a state, having section name as name of the state.

Fields:

- **`clip`**
    + If it is a string, it is a path to a `.webp` file, relative to the current `.toml` file.
    + If it is an inline table, it may contain the following keys:
        * `path` (string, required) – relative path to the `.webp` file
        * `weight` (integer, optional, default: 1) – used when a state has multiple clips, to influence random selection
        * `loop_count` (integer, optional) – overrides loop_count from `.webp` file, specifying the number of times the clip should play before evaluating transitions.
- **`[state_name]`**
    + Transition fields are named after other states.
    + The value is a TCE.
    + Sensing values can be read using _identifier path_ s.
    + Expression may include `$param_key` to be replaced with the parameter value (specified in the `list.toml`)

**Transition evaluation rules:**

1. When a clip completes its loop count, state machine evaluates all transitions in order of appearance.
2. The first transition whose expression evaluates to `true` is selected.
3. If transitions were taken, the widget remains in the current state (randomly selecting a clip again if multiple are defined).

> Transition chaining:
>    - If the destination state selects a clip with `loop_count = 0` and has an available transition, the next transition is immediately occurs.
>    - Chaining hops up to the number of states. After that, selected clip will be used at least once regardless of its loop_count.

**Available Sensing Values:**

Thease are bulitin sensing keys. More keys will be available via future updates and plugins.
- `cpu`: Total cpu usage (%, 0 to 1)
    + `.num`: Number of cpu cores
    + `.{i}` Cpu usage of `i`'th core (up to 255 cores)
- `mem`: Memory usage (%, 0 to 1)
    + `.total`: Total memory size (bytes)
    + `.avail`: Available memory size (bytes)
- `disk`
    + `.read`: total disk read speed (bytes/s)
    + `.write`: total disk write speed (bytes/s)
    + `.{drivelabel}`: if the drive with `drivelabel` is detected (boolean)
        * `.read`: disk read speed for drive with `drivelabel` (bytes/s)
        * `.write`: disk write speed for drive with `drivelabel` (bytes/s)
- `net`:
    + `.up`: total network upload speed (bytes/s)
    + `.down`: total network download speed (bytes/s)
    + `.{interfacename}`: if the interface with `interfacename` is present (boolean)
        * `.up`: network uploadspeed for interface with `interfacename` (bytes/s)
        * `.down`: network download speed for interface with `interfacename` (bytes/s)

All above sensing keys except for `disk.{disklabel}` and `net.{interfacename}`, . can have `.ema.{p}` suffix. It provides exponential moving average of the value with weight `0.p`. (e.g. `cpu.3.ema.02` will return usage of 3rd core, with ema coefficient `0.02`)

```toml
# exampls
[load0]
clip = "bored.webp"
load1 = "$sensor > 0.2" # using $sensor parameter

[load1]
# this clip will be skipped when transition arrives at this state with
# $sensor value < 0.2 or > 0.4
clip = { path = "walk.webp", loop_count = 0 }
load0 = "$sensor < 0.2"
load2 = "$sensor > 0.4"
```

### `sprites/running`

This file is used to indicate that the program is currently running.

- created on program startup.
- removed on program shutdown.
- Delete it while the program is running to shut down immediately.

### `plugins/`

Plugins extend the sensing sources, by making additional metrics available for use in TCEs.
Whenever new shared libraries are put into `plugins` directory, the program restarts to lo load them.

They typically provide system metrics
 can supply **any value**, depending on their implementation. Examples includes but not limited to:
- Email notification status
- Today’s weather
- Calendar events or reminders

See [Plugins](#Plugins-1) for detailed information on writing and integrating plugins.

Note that any shared library files placed in `plugins` are continuously detected & loaded. So be careful not to put untrusty files in.

### `misc/config.toml`

This file contains non-widget settings.
It is not monitored for updated, you need to manually restart the program to apply updates made in the config.

Fidls:
- **`max-log-level`**: maximum level of log messages to produce.
  - Type: `string`
  - Default: `"warn"`
  - Allowed values: `off`, `error`, `warn`, `info`, `debug`, `trace`
  - Higher levels produce more detailed logs.

- **`num-log-files`**: number of log files to keep.
    - Type: `number`
    - Default: `10`
    - Provided values will be floored, clamped between 0 and 100

- **`online-decoding`**: controls how sprite clips are loaded and decoded.
    - Type: `boolean`
    - Default: `false`
    - when **`false`**: Clips are fully decoded, and all frames are loaded into memory at startup.
    - when **`true`**: Clips are loaded in compressed form, and frames are decoded on demand when displayed.
    - Adjust `online-decoding` if you want to reduce memory usage at the cost of slightly higher CPU load when rendering clips.

### Log Files (`misc/*.log`)

On startup, a `mist/now.log` is created to output messages.

- Logs capture all messages from the program, including:
    + Clip load error messages
    + Syntax errors in TOML configuration files
    + Other runtime diagnostics
    + ... and so much more!
- If something isn’t working as expected, **check the log file first**.
- You may **keep the log file open** while making any changes to the `app_root` directory** to monitor runtime messages in real time.
- A maximum of (by default) 10 log files is kept having epoch time in the filename. Older logs are automatically deleted.
- Note that some messages will always be written directly to stdout. Run the program from the terminal to check them. The cases include:
    + Help message from `--help` command line argument
    + App info from `--info` command line argument
    + `app_root` directory lock failure message
    + Errors during App Path initialization
    + All messages when log file could not be crated

### Advanced (hidden) files

- `misc/.lock`: Exclusive file lock that allows only one mythic process access `app_root`. If you know there's mythic process running but keep getting directory lock error, consider removing this file manually.
- `./.templ`: Directory to keep loaded plugin libraries. Automatically managed.

## Command Line Arguments

Some settings are configured via commandline arguments. e.g. overriding `app_root`, prevent writing to `now.log`.
Run the program with `--help` for more info.


# Plugins

Plugins extend the sensing metrics system. They provide sensing values that can be used during sprite transition condition evaluation.

Plugins are implemented as shared libraries located in the plugins/ directory. The plugin name is derived from the library filename (without extension) and is used as the root namespace for sensing identifiers. e.g.  `cpu.core.nums`
`<plugin-name>.<identifier-path>`

The system resolves the plugin using the prefix and passes the remaining identifier path to the plugin during registration.

## ABI Interface

Plugins must expose a global symbol named `get_vtable`.
When called with no argument, it should return static pointer to a ABI table.

```c
#include <stdint.h>

typedef struct ABI {
    uint32_t magic;
    uint8_t  version_major;
    uint8_t  version_minor;
    uint8_t  tier;

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
    // or 0 (default) to refer to the whole identifier path.
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
```

## Compatibility

The ABI contains metadata for compatibility check:

| Field           | Requirements |
| --------------- | ------- |
| `magic`         | must be 0x5ABAD0B1 |
| `version_major` | breaking ABI change, must match with the host |
| `version_minor` | backward compatible change, host must have later version |

## Plugin Lifecycle

The host interacts with plugins using the following lifecycle:

1. Create instance (`create`)
    The instance will be used throughout the lifetime. If creation fails, the function must return NULL.

2. Register identifiers (`register`)
    This gets called for any identifier found in TCEs. Should return the sensng ID.

3. Periodic refresh (`refresh`)
    The plugins poll external data sources and update cached sensing values.

4. Read values
    The system read values from instance pointer using offset found in sensing_id.
    This gets called when evaluating TCEs.

5. Unregister identifiers (`unregister`)
    Called when TCE holding a sensing ID is unloaded.

6. Destroy instance (`destry`)
    The plugin must release all allocated resources.

Points of concern:
- One could manage refcount for sensing IDs (with `regiseter` and `unregister`) and only poll required metrics during the refresh.
- All the method calls are mutually exclusive.
- If `register` sucesses, read of the value should be valid. Make sure to poll & cache the value after registration of new metric.
- Identifier path string given to `register` must not be modified or kept. The string is only valid for the duration of the register call.
- Critical errors (memory corruption, invalid pointer access, etc) in plugins will directly effect the host. Do **everything** to remain safe.
- If `message` fails, its returned error code will not be processed further, and will be reported as a bug. Don't try to recover failures in this method. (e.g. giving message "unknown errcode")
- There will only be single instance created at at time for each loaded plugin. So you can use static buffer for instance data.

## Refreshing Period

Plugins can indicate expected update period of refreshing via `tier` field in ABI table.
```
period = 2^(tier-10) seconds
```

The host scheduler determines the exact timing; it will select smallest interval schedule period that is longer than requested.
For example, if the host have 9 (0.5s), 12 (4s), 20 (1024s) tier schedules, `refresh` of plugin with tier=10 will be called every 4 seconds.

## Minimal Example

See [counter.c][examples/plugins/counter.c]. You can build it with following using gcc:
```bash
# on linux
gcc -c -Wall -Werror -fPIC counter.c -shared -o counter.so
# on windows
gcc -shared -o counter.dll counter.c
```

When loaded, this plugin will provide three sensing metrics
- `counter.sec.1`: seconds elapsed since plugin load (float)
- `counter.sec.10`: seconds elapsed since plugin load, devided by 10 (float)
- `counter.over5s`: true if 5 seconds has passed since load (boolean)

The plugin increament internal timer with every refresh, skipping when refcount is 0.

<!-- # Performance -- TODO -->

# License

This project is licensed under the GNU GPL v3.0 or later.
See LICENSE.txt for details.


# TODO

- [ ] loop_count override
    + currently Clip holds loop_count. individual sprites cannot override it
- [ ] per clip position offset
    + enable specifying clip offsets from sprites position
    + allows fine-grained tuning when assembling clips
    + allows 'moving' sprites
- [ ] flexible sprite size
    + allowing autosize (omitting width/height or both) will allow clips of a sprite to have different sizes, which will make rendering buffer allocation hard.
    + currently both width and height need to be specified. all clips will be rescaled to match this size.
    + so, all clips of a sprite better have same aspect ratio.
- [ ] allow sprite state without clip
    + have 'empty' clip and with loop-count 0
- [ ] online decoding config
- [ ] rescale method config
- [ ] numeric parameters
- [ ] partial update when plugin updates
- [ ] partial update when list.toml updates
- [ ] partial update when sprite.toml updates
- [ ] plugin refresh tier
    + currently, the tier is overridden to 9
- [ ] smart name recognition for builtin sensors
    + currently, builtin sensors requires **exact** device name / interface name that pdh use
- [ ] more conservative logs
    + panic!/expect does not leave messages to log file.
    + manage separate log_details file to keep every level of log?