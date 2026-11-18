//! The built-in kinds' code identities (ADR 0009 §5), registered the same
//! way as a project's own kinds.

fn main() {
    // What every kind's pictures go through.
    const SHARED: [&str; 4] = [
        "src/kinds/mod.rs",
        "src/raster.rs",
        "src/color.rs",
        "src/curve.rs",
    ];
    let with = |own: &'static str| -> Vec<&'static str> {
        let mut v = vec![own];
        v.extend(SHARED);
        v
    };
    videbimus_build::Kinds::new()
        .kind("sequence", &with("src/kinds/sequence.rs"))
        .kind("stack", &with("src/kinds/stack.rs"))
        .kind("crossfade", &with("src/kinds/stack.rs"))
        .kind("filter", &with("src/kinds/filter.rs"))
        .kind("frame-player", &with("src/kinds/frame_player.rs"))
        .kind("image", &with("src/kinds/picture.rs"))
        .kind("caption", &with("src/kinds/caption.rs"))
        .write();
}
