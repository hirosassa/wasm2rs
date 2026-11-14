fn i16x8_relaxed_dot_i8x16_i7x16_s(a: u128, b: u128) -> u128 {
    let a = a.to_le_bytes();
    let b = b.to_le_bytes();
    let mut r = [0u8; 16];
    let mut o = 0;
    let mut s = 0;
    while o < 16 {
        let p0 = (a[s] as i8 as i32) * (b[s] as i8 as i32);
        let p1 = (a[s + 1] as i8 as i32) * (b[s + 1] as i8 as i32);
        let v = (p0 + p1).clamp(-32768, 32767) as i16;
        r[o..o + 2].copy_from_slice(&v.to_le_bytes());
        o += 2;
        s += 2;
    }
    u128::from_le_bytes(r)
}
