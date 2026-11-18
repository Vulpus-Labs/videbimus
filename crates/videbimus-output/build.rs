//! The output recipes' code identities (ADR 0009 §5): editing how a
//! segment is rendered or encoded re-renders the segments; editing the
//! join re-joins.

fn main() {
    videbimus_build::Kinds::new()
        .kind(
            "output-segment",
            &["src/segment.rs", "src/encode.rs", "src/state.rs"],
        )
        .kind("output-join", &["src/join.rs", "src/encode.rs"])
        .write();
}
