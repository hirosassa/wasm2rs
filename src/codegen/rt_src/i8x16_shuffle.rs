fn i8x16_shuffle(a: u128, b: u128, idx: u128) -> u128 {
    let a = a.to_le_bytes();
    let b = b.to_le_bytes();
    let idx = idx.to_le_bytes();
    let mut r = [0u8; 16];
    let mut i = 0;
    while i < 16 {
        let j = idx[i] as usize;
        r[i] = if j < 16 {
            a[j]
        } else if j < 32 {
            b[j - 16]
        } else {
            0
        };
        i += 1;
    }
    u128::from_le_bytes(r)
}
