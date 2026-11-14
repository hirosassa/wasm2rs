fn i32x4_relaxed_dot_i8x16_i7x16_add_s(a: u128, b: u128, c: u128) -> u128 {
    let a = a.to_le_bytes();
    let b = b.to_le_bytes();
    let c = c.to_le_bytes();
    let mut r = [0u8; 16];
    let mut o = 0;
    let mut s = 0;
    while o < 16 {
        let d0 = ((a[s] as i8 as i32) * (b[s] as i8 as i32)
            + (a[s + 1] as i8 as i32) * (b[s + 1] as i8 as i32))
            .clamp(-32768, 32767);
        let d1 = ((a[s + 2] as i8 as i32) * (b[s + 2] as i8 as i32)
            + (a[s + 3] as i8 as i32) * (b[s + 3] as i8 as i32))
            .clamp(-32768, 32767);
        let acc = i32::from_le_bytes(c[o..o + 4].try_into().unwrap());
        let v = acc.wrapping_add(d0).wrapping_add(d1);
        r[o..o + 4].copy_from_slice(&v.to_le_bytes());
        o += 4;
        s += 4;
    }
    u128::from_le_bytes(r)
}
