//! The built-in recipe kinds' code identities (ADR 0009 §5), registered
//! the same way as a project's own kinds.

fn main() {
    videbimus_build::Kinds::new()
        .kind("command", &["src/command"])
        .write();
}
