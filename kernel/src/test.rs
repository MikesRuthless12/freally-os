// SPDX-License-Identifier: Apache-2.0
//! The kernel's test harness (TASK-022).
//!
//! `ktest!(name, { … })` puts a test in the `.ktests` section. With the `ktest`
//! feature, the kernel runs every one after it has initialised, on every CPU
//! in turn (TASK-036: the test thread pinned to each; on CPU 1 and up a test's
//! name gets `@cpu<n>`), prints `ktest <module>::<name> … pass|fail (reason)`
//! on COM1, then exits QEMU
//! through `isa-debug-exit`: 0x10 (QEMU exit 33) when all pass, 0x11 (35) when
//! any fails. A kernel panic in a test is that test's failure.
//! `cargo xtask test` builds, boots and reads all of this.

/// One test.
#[repr(C)]
pub struct KTest {
    pub name: &'static str,
    pub module: &'static str,
    pub run: fn() -> Result<(), &'static str>,
}

#[macro_export]
macro_rules! ktest {
    ($name:ident, $body:block) => {
        #[cfg(feature = "ktest")]
        #[allow(non_upper_case_globals)]
        #[used]
        #[unsafe(link_section = ".ktests")]
        static $name: $crate::test::KTest = $crate::test::KTest {
            name: stringify!($name),
            module: module_path!(),
            run: {
                fn t() -> Result<(), &'static str> $body
                t
            },
        };
    };
}

#[cfg(feature = "ktest")]
mod run {
    use super::KTest;
    use crate::arch::cpu;
    use crate::fmtbuf::Buf;
    use core::sync::atomic::{AtomicUsize, Ordering};

    unsafe extern "C" {
        // Bytes, not KTests: only their addresses are used.
        static __ktests_start: u8;
        static __ktests_end: u8;
    }

    static CURRENT: AtomicUsize = AtomicUsize::new(usize::MAX);
    static ON: AtomicUsize = AtomicUsize::new(0);
    static PASSED: AtomicUsize = AtomicUsize::new(0);

    /// The CPU a test ran on, as its name's suffix (none on CPU 0).
    struct On(usize);

    impl core::fmt::Display for On {
        fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
            if self.0 == 0 { Ok(()) } else { write!(f, "@cpu{}", self.0) }
        }
    }

    fn all() -> &'static [KTest] {
        // SAFETY: the linker script gathers every KTest between these symbols.
        unsafe {
            let start = (&raw const __ktests_start) as *const KTest;
            let end = (&raw const __ktests_end) as *const KTest;
            core::slice::from_raw_parts(start, end.offset_from(start) as usize)
        }
    }

    fn short(module: &str) -> &str {
        module.strip_prefix("freally_core::").unwrap_or(module)
    }

    pub fn run_all() -> ! {
        let tests = all();
        let cpus = crate::smp::online();
        klog!("ktests: {} to run, on each of {} CPUs", tests.len(), cpus);
        let mut failed = 0;
        for on in 0..cpus {
            ON.store(on, Ordering::SeqCst);
            if !crate::sched::pin_current(on) {
                failed += tests.len();
                klog!("ktest smp::pin_current{} … fail\n    the tests couldn't move to CPU {}", On(on), on);
                continue;
            }
            for (i, t) in tests.iter().enumerate() {
                CURRENT.store(i, Ordering::SeqCst);
                let start = cpu::rdtsc();
                let r = (t.run)();
                let ticks = cpu::rdtsc() - start;
                match r {
                    Ok(()) => {
                        PASSED.fetch_add(1, Ordering::SeqCst);
                        klog!("ktest {}::{}{} … pass ({} Mticks)", short(t.module), t.name, On(on), ticks / 1_000_000);
                    }
                    Err(why) => {
                        failed += 1;
                        klog!("ktest {}::{}{} … fail\n    {}", short(t.module), t.name, On(on), why);
                    }
                }
            }
        }
        let total = tests.len() * cpus;
        klog!("ktests: {} of {} pass", total - failed, total);
        cpu::qemu_exit(if failed == 0 { 0x10 } else { 0x11 })
    }

    /// A panic or fault during a test fails it (and ends the run).
    pub fn panicked(what: &str) {
        let tests = all();
        let i = CURRENT.load(Ordering::SeqCst);
        if let Some(t) = tests.get(i) {
            let mut b = Buf::<200>::new();
            let _ = core::fmt::write(&mut b, format_args!("{what}"));
            let on = ON.load(Ordering::SeqCst);
            klog!("ktest {}::{}{} … fail\n    panicked: {}", short(t.module), t.name, On(on), b.as_str());
            klog!("ktests: {} of {} pass", PASSED.load(Ordering::SeqCst), tests.len() * crate::smp::online());
            cpu::qemu_exit(0x11);
        }
    }
}

#[cfg(feature = "ktest")]
pub use run::{panicked, run_all};

ktest!(harness_runs_tests, { Ok(()) });
