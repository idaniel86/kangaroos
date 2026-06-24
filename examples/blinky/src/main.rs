#![no_std]
#![no_main]

use cortex_m_rt::{entry, exception};
use core::panic::PanicInfo;

static mut STACK_A: [u32; 256] = [0; 256]; // 1 KiB
static mut STACK_B: [u32; 256] = [0; 256]; // 1 KiB

fn task_a() -> ! {
    let mut tick: u32 = 0;
    loop {
        tick = tick.wrapping_add(1);
        // Breakpoint in a debugger or replace with semihosting/GPIO toggle.
        cortex_m::asm::nop();
    }
}

fn task_b() -> ! {
    let mut tick: u32 = 0;
    loop {
        tick = tick.wrapping_add(1);
        cortex_m::asm::nop();
    }
}

#[entry]
fn main() -> ! {
    unsafe {
        kangaroos::spawn_task(&mut *core::ptr::addr_of_mut!(STACK_A), task_a);
        kangaroos::spawn_task(&mut *core::ptr::addr_of_mut!(STACK_B), task_b);
    }
    // lm3s811evb (QEMU Cortex-M3) runs at 8 MHz.
    // Change to match your board's CPU clock.
    kangaroos::kernel_start(8_000_000)
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