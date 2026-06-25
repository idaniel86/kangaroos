#![no_std]
#![no_main]

use cortex_m_rt::{entry, exception};
use cortex_m_semihosting::hprintln;
use core::panic::PanicInfo;
use kangaroos::sync::Semaphore;
use kangaroos::timer::Duration;

static mut STACK_A: [u32; 512] = [0; 512]; // 2 KiB
static mut STACK_B: [u32; 512] = [0; 512]; // 2 KiB
static mut KERNEL: kangaroos::Kernel<8> = kangaroos::Kernel::new();

// SEM_A starts at 1 so task_a fires first; SEM_B starts at 0.
static SEM_A: Semaphore = Semaphore::new(1, 1);
static SEM_B: Semaphore = Semaphore::new(0, 1);

fn task_a() -> ! {
    loop {
        SEM_A.take();
        hprintln!("tick");
        kangaroos::task::sleep(Duration::from_secs(1));
        SEM_B.give();
    }
}

fn task_b() -> ! {
    loop {
        SEM_B.take();
        hprintln!("tock");
        kangaroos::task::sleep(Duration::from_secs(1));
        SEM_A.give();
    }
}

#[entry]
fn main() -> ! {
    let k = unsafe { &mut *core::ptr::addr_of_mut!(KERNEL) };
    unsafe {
        kangaroos::task::spawn(k, &mut *core::ptr::addr_of_mut!(STACK_A), 0, 10, task_a);
        kangaroos::task::spawn(k, &mut *core::ptr::addr_of_mut!(STACK_B), 0, 10, task_b);
    }
    // lm3s811evb (QEMU Cortex-M3) runs at 8 MHz.
    // Change to match your board's CPU clock.
    k.start(8_000_000)
}

#[exception]
fn SysTick() {
    kangaroos::systick_handler();
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    loop {
        cortex_m::asm::bkpt();
    }
}