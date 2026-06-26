# kangaroos

A preemptive real-time operating system for ARM Cortex-M microcontrollers, written in Rust (`no_std`).

> **Note:** This project was created as an experiment using [GitHub Copilot](https://github.com/features/copilot) as an AI pair-programmer.

## Features

- Fixed-priority preemptive scheduling with round-robin tiebreaking at equal priorities
- Static task model — task count and stacks are fixed at compile time; no heap allocator required
- Full Cortex-M coverage: ARMv6-M (M0/M0+), ARMv7-M (M3/M4), ARMv7E-M+FPU (M4F/M7), ARMv8-M (M23/M33/M55)
- PendSV/SysTick-based context switch with proper priority separation
- Stack overflow detection via canary words (all targets), MPU guard region (M3/M4/M7), and `PSPLIM` (M23/M33/M55)
- Optional `defmt` logging with millisecond timestamps

## Workspace layout

```
kangaroos/
├── kangaroos/            # kernel crate
│   └── src/
│       ├── arch/         # ArchContext trait + per-variant implementations
│       ├── channel/      # MPMC bounded channel, ISR-safe send
│       ├── kernel/       # Kernel<N>, scheduler, TCB, idle task
│       ├── mem/          # Pool<T,N> — O(1) static allocator
│       ├── sync/         # Mutex (PI), Semaphore, Condvar, EventGroup, Once
│       ├── task.rs       # Spawner, SpawnToken, sleep, yield
│       └── timer/        # Duration, Instant, Timer
├── kangaroos-macros/     # #[task] and #[main] proc-macros
└── examples/
    └── blinky/           # tick/tock demo — runs on QEMU lm3s811evb
```

## Requirements

| Tool | Version |
|------|---------|
| Rust (stable) | ≥ 1.80 |
| `qemu-system-arm` | ≥ 8.x (for running examples) |
| `defmt-print` | any (optional, for decoded `defmt` output) |

Install the required cross-compilation targets once:

```sh
rustup target add \
  thumbv6m-none-eabi \
  thumbv7m-none-eabi \
  thumbv7em-none-eabihf \
  thumbv8m.base-none-eabi \
  thumbv8m.main-none-eabi \
  thumbv8m.main-none-eabihf
```

## Quick start

### Build

```sh
# ARMv6-M (Cortex-M0/M0+)
cargo build --target thumbv6m-none-eabi -p blinky

# ARMv7-M (Cortex-M3/M4)
cargo build --target thumbv7m-none-eabi -p blinky

# ARMv7E-M + FPU (Cortex-M4F/M7)
cargo build --target thumbv7em-none-eabihf -p blinky

# ARMv8-M Baseline (Cortex-M23) — Thumb-1 only, no FPU
cargo build --target thumbv8m.base-none-eabi -p blinky

# ARMv8-M Mainline (Cortex-M33/M55/M85) — Thumb-2, no FPU
cargo build --target thumbv8m.main-none-eabi -p blinky

# ARMv8-M Mainline + FPU (Cortex-M33F/M55F/M85)
cargo build --target thumbv8m.main-none-eabihf -p blinky
```

### Run on QEMU (plain semihosting output)

```sh
qemu-system-arm -M lm3s811evb \
  -semihosting-config enable=on,target=native \
  -nographic \
  -kernel target/thumbv7m-none-eabi/debug/blinky
```

Expected output (repeating):
```
tick
tock
tick
tock
```

### Run on QEMU with `defmt` output

```sh
cargo build --target thumbv7m-none-eabi -p blinky --features defmt

qemu-system-arm -M lm3s811evb \
  -semihosting-config enable=on,target=native \
  -kernel target/thumbv7m-none-eabi/debug/blinky 2>&1 | \
  defmt-print -e target/thumbv7m-none-eabi/debug/blinky
```

### GDB debugging (QEMU GDB stub)

```sh
# Terminal 1 — start QEMU, freeze at reset, expose GDB port 1234
qemu-system-arm -M lm3s811evb -nographic \
  -kernel target/thumbv7m-none-eabi/debug/blinky -s -S

# Terminal 2 — connect GDB
arm-none-eabi-gdb target/thumbv7m-none-eabi/debug/blinky \
  -ex "target remote :1234" -ex "load" -ex "continue"
```

## Writing an application

### 1. Declare tasks

Annotate functions with `#[kangaroos::task]`. Each function becomes a factory that returns a `SpawnToken`.

```rust
#[kangaroos::task(priority = 1, stack_size = 1024, time_slice = 10)]
fn blink(period_ms: u64) -> ! {
    loop {
        // toggle LED …
        kangaroos::task::sleep(Duration::from_millis(period_ms));
    }
}
```

| Attribute | Type | Description |
|-----------|------|-------------|
| `priority` | `u8` | Task priority (0 = highest) |
| `stack_size` | `usize` | Stack size in bytes |
| `time_slice` | `u8` | Round-robin time-slice in ticks (default 10 ms) |

### 2. Define the entry point

```rust
#[kangaroos::main(cpu_hz = 8_000_000, max_tasks = 4)]
fn main(spawner: &mut Spawner) {
    spawner.spawn(blink(500));
    spawner.spawn(monitor());
}
```

| Attribute | Type | Description |
|-----------|------|-------------|
| `cpu_hz` | `u32` | CPU clock frequency — programs the 1 ms SysTick period |
| `max_tasks` | `usize` | Maximum concurrent tasks (sets `Kernel<N>`) |

### 3. Hook SysTick

```rust
use cortex_m_rt::exception;

#[exception]
fn SysTick() {
    kangaroos::systick_handler();
}
```

### Complete example — `examples/blinky/src/main.rs`

```rust
#![no_std]
#![no_main]

use cortex_m_rt::exception;
use kangaroos::{timer::Duration, main, task, task::sleep, Spawner};

kangaroos::semaphore!(SEM_A, 1, 1);
kangaroos::semaphore!(SEM_B, 0, 1);

#[task(priority = 0, stack_size = 2048, time_slice = 10)]
fn task_a(secs: u64) -> ! {
    loop {
        SEM_A.take();
        // print "tick" …
        sleep(Duration::from_secs(secs));
        SEM_B.give();
    }
}

#[task(priority = 0, stack_size = 2048, time_slice = 10)]
fn task_b(secs: u64) -> ! {
    loop {
        SEM_B.take();
        // print "tock" …
        sleep(Duration::from_secs(secs));
        SEM_A.give();
    }
}

#[main(cpu_hz = 8_000_000, max_tasks = 2)]
fn main(spawner: &mut Spawner) {
    spawner.spawn(task_a(1));
    spawner.spawn(task_b(1));
}

#[exception]
fn SysTick() { kangaroos::systick_handler(); }
```

## Synchronization primitives

All primitives are zero-allocation `static` values. Convenience macros generate named statics for better debug output.

### Mutex

Priority-inheritance mutex protecting a `T`.

```rust
kangaroos::mutex!(COUNTER, u32, 0);

// In a task:
let mut guard = COUNTER.lock();
*guard += 1;
// guard drops → mutex released
```

### Semaphore

Counting semaphore with configurable maximum.

```rust
kangaroos::semaphore!(SEM, 0, 4);   // initial=0, max=4

SEM.give();          // increment (ISR-safe)
SEM.give_from_isr(); // ISR variant
SEM.take();          // blocking decrement
```

### Condvar

Condition variable paired with a `Mutex`.

```rust
kangaroos::mutex!(MX, bool, false);
kangaroos::condvar!(CV);

// Waiter task:
let mut guard = MX.lock();
CV.wait(&mut guard, |v| *v);  // blocks until predicate is true

// Signaller task:
let mut guard = MX.lock();
*guard = true;
CV.notify_one();
```

### EventGroup

32-bit flag word supporting wait-any and wait-all semantics.

```rust
kangaroos::event_group!(FLAGS);

// Set bits 0 and 1:
FLAGS.set(0b0011);

// Wait until both bits are set (cleared on exit):
FLAGS.wait_all(0b0011, true);
```

### Once

One-shot initialisation cell; blocks all callers until the first call completes.

```rust
kangaroos::once!(INIT);

INIT.call_once(|| {
    // run exactly once, even if multiple tasks race here
});
```

## IPC — Channel

Statically-allocated MPMC bounded channel. Capacity `N` is a const generic.

```rust
static CH: kangaroos::channel::Channel<u32, 8> = kangaroos::channel::Channel::new();

// Producer:
let tx = CH.sender();
tx.send(42);          // blocking
tx.try_send(42);      // non-blocking

// Consumer:
let rx = CH.receiver();
let val = rx.recv();       // blocking
let val = rx.try_recv();   // non-blocking → Option<u32>
```

## Memory — Pool

O(1) fixed-capacity slab allocator. No heap required.

```rust
static POOL: kangaroos::Pool<[u8; 64], 4> = kangaroos::Pool::new();

// Non-blocking:
if let Some(mut buf) = POOL.alloc([0u8; 64]) {
    buf[0] = 0xAB;
}   // buf dropped → slot automatically returned

// Blocking (task context only):
let buf = POOL.alloc_blocking([0u8; 64]);
```

## Time API

```rust
use kangaroos::timer::{Duration, Instant, Timer};

// Sleep for 500 ms:
kangaroos::task::sleep(Duration::from_millis(500));

// One-shot timer (fires once after 1 s):
let t = Timer::after(Duration::from_secs(1));
t.wait();

// Periodic timer (fires every 100 ms):
let mut t = Timer::every(Duration::from_millis(100));
loop {
    t.wait();
    // do periodic work
}

// Read the monotonic clock:
let start = Instant::now();
// …
let elapsed: Duration = Instant::now() - start;
```

The tick rate defaults to **1 kHz** (1 tick = 1 ms). Change `timer::TICKS_PER_SEC` if you configure a different SysTick period.

## Scheduler

- **Algorithm**: fixed-priority preemptive with round-robin within equal-priority tiers
- **Tick**: 64-bit monotonic counter incremented in `SysTick` at 1 kHz
- **Context switch**: PendSV at the lowest interrupt priority (0xFF); SysTick at the highest (0x00)
- **Maximum tasks**: `N - 1` user tasks + 1 idle task; `N ≤ 254`

## Stack overflow detection

| Target | Mechanism |
|--------|-----------|
| All | Canary (`0xDEAD_BEEF × 4`) at stack bottom, checked every SysTick |
| ARMv7-M (M3/M4/M7) | MPU no-access guard region (32 B), reprogrammed in PendSV |
| ARMv8-M (M23/M33/M55) | `PSPLIM` register updated on every context switch |

## Optional features

| Feature | Description |
|---------|-------------|
| `defmt` | Enable structured logging via `defmt`; adds millisecond timestamps |

Enable in `Cargo.toml`:

```toml
[dependencies]
kangaroos = { path = "../kangaroos", features = ["defmt"] }
```

## Design constraints

- **No heap** — all data structures are statically allocated
- **No async** — classic blocking RTOS style; no executor
- **No vendor HAL** — bring your own peripheral drivers
- **Single-core only** — all critical sections use `interrupt::free`

## License

TBD
