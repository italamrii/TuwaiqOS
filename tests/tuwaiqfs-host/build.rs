//! Regenerates compilable copies of the kernel's `tuwaiqfs.rs` and `fs.rs`
//! on every build, so the tests always run against the current kernel source.
//!
//! The ONLY transformation is turning inner doc comments (`//!`) into ordinary
//! comments (`//`): `include!` cannot expand inner attributes into the middle
//! of a module body. Comments carry no semantics, so the code under test is
//! otherwise byte-for-byte what the kernel compiles.
//!
//! This is what keeps the tests honest — there is no copy of the filesystem
//! logic in this crate that could quietly drift out of sync with `kernel/src`.

use std::env;
use std::fs;
use std::path::PathBuf;

/// Kernel modules pulled in verbatim, in dependency order.
const MODULES: &[&str] = &["tuwaiqfs", "fs"];

fn main() {
    let kernel_src = kernel_src_dir();
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let mut code_lines = 0usize;

    for module in MODULES {
        let source = kernel_src.join(format!("{module}.rs"));
        println!("cargo:rerun-if-changed={}", source.display());

        let text = fs::read_to_string(&source).unwrap_or_else(|err| {
            panic!(
                "cannot read {}: {err}\n\
                 Run these tests from a full checkout, or set KERNEL_SRC.",
                source.display()
            )
        });

        let (converted, lines) = strip_inner_docs(&text);
        code_lines += lines;
        fs::write(out_dir.join(format!("{module}_gen.rs")), converted)
            .unwrap_or_else(|err| panic!("cannot write generated {module}: {err}"));
    }

    println!("cargo:rerun-if-env-changed=KERNEL_SRC");
    println!("cargo:warning=testing {code_lines} lines of kernel source verbatim");
}

/// `KERNEL_SRC` if set, otherwise `../../kernel/src` relative to this crate.
fn kernel_src_dir() -> PathBuf {
    if let Ok(dir) = env::var("KERNEL_SRC") {
        return PathBuf::from(dir);
    }
    PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("kernel")
        .join("src")
}

/// Rewrite `//!` to `//`, and count the lines of real code carried over.
fn strip_inner_docs(text: &str) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut code_lines = 0usize;

    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("//!") {
            out.push_str(&line[..line.len() - trimmed.len()]);
            out.push_str("//");
            out.push_str(rest);
        } else {
            if !trimmed.is_empty() && !trimmed.starts_with("//") {
                code_lines += 1;
            }
            out.push_str(line);
        }
        out.push('\n');
    }

    (out, code_lines)
}
