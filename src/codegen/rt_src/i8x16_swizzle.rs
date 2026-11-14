fn i8x16_swizzle(a: u128, b: u128) -> u128 {
    let a = a.to_le_bytes();
    let b = b.to_le_bytes();
    let mut r = [0u8; 16];
    let mut i = 0;
    while i < 16 {
        let j = b[i] as usize;
        if j < 16 {
            r[i] = a[j];
        }
        i += 1;
    }
    u128::from_le_bytes(r)
}
