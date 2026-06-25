#![no_std]
#![no_main]

use cortex_m_rt::exception;
#[cfg(feature = "defmt")]
use defmt_semihosting as _;  // pull in transport
#[cfg(not(feature = "defmt"))]
use cortex_m_semihosting::hprintln;
use core::panic::PanicInfo;
use kangaroos::{sync::Semaphore, timer::Duration, main, task, task::sleep, Spawner};

// SEM_A starts at 1 so task_a fires first; SEM_B starts at 0.
static SEM_A: Semaphore = Semaphore::new(1, 1);
static SEM_B: Semaphore = Semaphore::new(0, 1);

#[task(priority = 0, stack_size = 2048, time_slice = 10)]
fn task_a(secs: u64) -> ! {
    loop {
        SEM_A.take();
        #[cfg(not(feature = "defmt"))]
        hprintln!("tick");
        #[cfg(feature = "defmt")]
        defmt::info!("tick");
        sleep(Duration::from_secs(secs));
        SEM_B.give();
    }
}

#[task(priority = 0, stack_size = 2048, time_slice = 10)]
fn task_b(secs: u64) -> ! {
    loop {
        SEM_B.take();
        #[cfg(not(feature = "defmt"))]
        hprintln!("tock");
        #[cfg(feature = "defmt")]
        defmt::info!("tock");
        sleep(Duration::from_secs(secs));
        SEM_A.give();
    }
}

/// lm3s811evb (QEMU Cortex-M3) runs at 8 MHz.
/// Change `cpu_hz` to match your board's CPU clock.
#[main(cpu_hz = 8_000_000, max_tasks = 2)]
fn main(spawner: &mut Spawner) {
    spawner.spawn(task_a(1));
    spawner.spawn(task_b(1));
}

#[exception]
fn SysTick() {
    kangaroos::systick_handler();
}

#[cfg(feature = "defmt")]
#[defmt::panic_handler]
fn defmt_panic() -> ! {
    cortex_m::asm::bkpt();
    loop {}
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    loop {
        cortex_m::asm::bkpt();
    }
}
