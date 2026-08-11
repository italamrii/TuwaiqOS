//! A stand-in for the `x86_64` crate, so `fs.rs` compiles unchanged on the host.
//!
//! `fs.rs` guards its critical sections with
//! `x86_64::instructions::interrupts::without_interrupts`. The real crate
//! executes `cli` and `sti`, which are privileged: running them from a host
//! test process raises `SIGSEGV` rather than masking anything.
//!
//! Depending on the real crate and hoping those paths are never reached is not
//! an option either, since every mutating call in `fs.rs` goes through one.
//!
//! What the kernel gets from `without_interrupts` is the guarantee that no
//! other execution context observes a half-updated filesystem. On the host,
//! `TEST_LOCK` in the rig provides that same guarantee by serialising the
//! tests, so the closure can simply be called.
//!
//! The narrowing this introduces is worth stating plainly: these tests cannot
//! detect a bug that only appears when an interrupt lands inside one of those
//! sections. They test the filesystem logic, not the kernel's interrupt
//! discipline.

pub mod instructions {
    pub mod interrupts {
        /// Run `f`. See the module docs for why this is not a no-op by accident.
        #[inline]
        pub fn without_interrupts<F, R>(f: F) -> R
        where
            F: FnOnce() -> R,
        {
            f()
        }
    }
}
