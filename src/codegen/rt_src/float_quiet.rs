fn quiet_f32(r: f32) -> f32 {
    if r.is_nan() { r + r } else { r }
}
