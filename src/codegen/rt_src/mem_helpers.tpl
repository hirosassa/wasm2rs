// Templates of the linear-memory helper methods, one per section: `//# <method>`
// starts a section that runs to the next blank line. `@NAME` is the method name,
// `@R` / `@W` the read / write accessors of the memory bytes, and `@ADDR` /
// `@OFF` / `@EA` the address operand types and effective address. The
// placeholders make these lines invalid Rust, so unlike the other `rt_src` files
// they are not compiled here; the rendered methods are compiled by the e2e tests.
// `memory.grow` depends on the memory's bound, so it stays in `helpers.rs`.

//# r32
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> i32 {
    let a = @EA;
    i32::from_le_bytes(@R[a..a + 4].try_into().unwrap())
}

//# r8u
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> i32 {
    let a = @EA;
    @R[a] as i32
}

//# r8s
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> i32 {
    let a = @EA;
    @R[a] as i8 as i32
}

//# r16u
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> i32 {
    let a = @EA;
    u16::from_le_bytes(@R[a..a + 2].try_into().unwrap()) as i32
}

//# r16s
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> i32 {
    let a = @EA;
    i16::from_le_bytes(@R[a..a + 2].try_into().unwrap()) as i32
}

//# w32
fn @NAME(&mut self, addr: @ADDR, offset: @OFF, value: i32) {
    let a = @EA;
    @W[a..a + 4].copy_from_slice(&value.to_le_bytes());
}

//# w8
fn @NAME(&mut self, addr: @ADDR, offset: @OFF, value: i32) {
    let a = @EA;
    @W[a] = value as u8;
}

//# w16
fn @NAME(&mut self, addr: @ADDR, offset: @OFF, value: i32) {
    let a = @EA;
    @W[a..a + 2].copy_from_slice(&(value as u16).to_le_bytes());
}

//# r64
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> i64 {
    let a = @EA;
    i64::from_le_bytes(@R[a..a + 8].try_into().unwrap())
}

//# rf32
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> f32 {
    let a = @EA;
    f32::from_le_bytes(@R[a..a + 4].try_into().unwrap())
}

//# rf64
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> f64 {
    let a = @EA;
    f64::from_le_bytes(@R[a..a + 8].try_into().unwrap())
}

//# r8u64
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> i64 {
    let a = @EA;
    @R[a] as i64
}

//# r8s64
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> i64 {
    let a = @EA;
    @R[a] as i8 as i64
}

//# r16u64
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> i64 {
    let a = @EA;
    u16::from_le_bytes(@R[a..a + 2].try_into().unwrap()) as i64
}

//# r16s64
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> i64 {
    let a = @EA;
    i16::from_le_bytes(@R[a..a + 2].try_into().unwrap()) as i64
}

//# r32u64
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> i64 {
    let a = @EA;
    u32::from_le_bytes(@R[a..a + 4].try_into().unwrap()) as i64
}

//# r32s64
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> i64 {
    let a = @EA;
    i32::from_le_bytes(@R[a..a + 4].try_into().unwrap()) as i64
}

//# w64
fn @NAME(&mut self, addr: @ADDR, offset: @OFF, value: i64) {
    let a = @EA;
    @W[a..a + 8].copy_from_slice(&value.to_le_bytes());
}

//# wf32
fn @NAME(&mut self, addr: @ADDR, offset: @OFF, value: f32) {
    let a = @EA;
    @W[a..a + 4].copy_from_slice(&value.to_le_bytes());
}

//# wf64
fn @NAME(&mut self, addr: @ADDR, offset: @OFF, value: f64) {
    let a = @EA;
    @W[a..a + 8].copy_from_slice(&value.to_le_bytes());
}

//# w8_64
fn @NAME(&mut self, addr: @ADDR, offset: @OFF, value: i64) {
    let a = @EA;
    @W[a] = value as u8;
}

//# w16_64
fn @NAME(&mut self, addr: @ADDR, offset: @OFF, value: i64) {
    let a = @EA;
    @W[a..a + 2].copy_from_slice(&(value as u16).to_le_bytes());
}

//# w32_64
fn @NAME(&mut self, addr: @ADDR, offset: @OFF, value: i64) {
    let a = @EA;
    @W[a..a + 4].copy_from_slice(&(value as u32).to_le_bytes());
}

// A v128 load/store moves 16 bytes to/from memory as a little-endian
// `u128`; the slice access is bounds-checked, so an out-of-range access
// panics (a wasm trap).
//# rv128
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> u128 {
    let a = @EA;
    let mut b = [0u8; 16];
    b.copy_from_slice(&@R[a..a + 16]);
    u128::from_le_bytes(b)
}

//# wv128
fn @NAME(&mut self, addr: @ADDR, offset: @OFF, value: u128) {
    let a = @EA;
    @W[a..a + 16].copy_from_slice(&value.to_le_bytes());
}

// `load*_splat` reads one element and broadcasts it to every lane;
// `load*_zero` reads one into the low lane and zeroes the rest. Each
// access is bounds-checked, so an out-of-range read traps.
//# load8_splat
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> u128 {
    let a = @EA;
    u128::from_le_bytes([@R[a]; 16])
}

//# load16_splat
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> u128 {
    let a = @EA;
    let mut lane = [0u8; 2];
    lane.copy_from_slice(&@R[a..a + 2]);
    let mut b = [0u8; 16];
    let mut i = 0;
    while i < 16 {
        b[i..i + 2].copy_from_slice(&lane);
        i += 2;
    }
    u128::from_le_bytes(b)
}

//# load32_splat
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> u128 {
    let a = @EA;
    let mut lane = [0u8; 4];
    lane.copy_from_slice(&@R[a..a + 4]);
    let mut b = [0u8; 16];
    let mut i = 0;
    while i < 16 {
        b[i..i + 4].copy_from_slice(&lane);
        i += 4;
    }
    u128::from_le_bytes(b)
}

//# load64_splat
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> u128 {
    let a = @EA;
    let mut b = [0u8; 16];
    b[0..8].copy_from_slice(&@R[a..a + 8]);
    b.copy_within(0..8, 8);
    u128::from_le_bytes(b)
}

//# load32_zero
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> u128 {
    let a = @EA;
    let mut b = [0u8; 16];
    b[0..4].copy_from_slice(&@R[a..a + 4]);
    u128::from_le_bytes(b)
}

//# load64_zero
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> u128 {
    let a = @EA;
    let mut b = [0u8; 16];
    b[0..8].copy_from_slice(&@R[a..a + 8]);
    u128::from_le_bytes(b)
}

// `load{8x8,16x4,32x2}_{s,u}` read eight bytes and widen each source lane
// to a double-width lane: `_s` sign-extends, `_u` zero-extends. The eight
// bytes are bounds-checked, so an out-of-range read traps.
//# load8x8_s
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> u128 {
    let a = @EA;
    let mut r = [0u8; 16];
    let mut i = 0;
    while i < 8 {
        let x = @R[a + i] as i8 as i16;
        r[i * 2..i * 2 + 2].copy_from_slice(&x.to_le_bytes());
        i += 1;
    }
    u128::from_le_bytes(r)
}

//# load8x8_u
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> u128 {
    let a = @EA;
    let mut r = [0u8; 16];
    let mut i = 0;
    while i < 8 {
        let x = @R[a + i] as u16;
        r[i * 2..i * 2 + 2].copy_from_slice(&x.to_le_bytes());
        i += 1;
    }
    u128::from_le_bytes(r)
}

//# load16x4_s
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> u128 {
    let a = @EA;
    let mut r = [0u8; 16];
    let mut i = 0;
    while i < 4 {
        let s = a + i * 2;
        let x = i16::from_le_bytes([@R[s], @R[s + 1]]) as i32;
        r[i * 4..i * 4 + 4].copy_from_slice(&x.to_le_bytes());
        i += 1;
    }
    u128::from_le_bytes(r)
}

//# load16x4_u
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> u128 {
    let a = @EA;
    let mut src = [0u8; 8];
    src.copy_from_slice(&@R[a..a + 8]);
    let mut r = [0u8; 16];
    let mut i = 0;
    while i < 4 {
        let s = i * 2;
        let x = u16::from_le_bytes([src[s], src[s + 1]]) as u32;
        r[i * 4..i * 4 + 4].copy_from_slice(&x.to_le_bytes());
        i += 1;
    }
    u128::from_le_bytes(r)
}

//# load32x2_s
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> u128 {
    let a = @EA;
    let mut r = [0u8; 16];
    let mut i = 0;
    while i < 2 {
        let s = a + i * 4;
        let x = i32::from_le_bytes([@R[s], @R[s + 1], @R[s + 2], @R[s + 3]]) as i64;
        r[i * 8..i * 8 + 8].copy_from_slice(&x.to_le_bytes());
        i += 1;
    }
    u128::from_le_bytes(r)
}

//# load32x2_u
fn @NAME(&self, addr: @ADDR, offset: @OFF) -> u128 {
    let a = @EA;
    let mut src = [0u8; 8];
    src.copy_from_slice(&@R[a..a + 8]);
    let mut r = [0u8; 16];
    let mut i = 0;
    while i < 2 {
        let s = i * 4;
        let x = u32::from_le_bytes([src[s], src[s + 1], src[s + 2], src[s + 3]]) as u64;
        r[i * 8..i * 8 + 8].copy_from_slice(&x.to_le_bytes());
        i += 1;
    }
    u128::from_le_bytes(r)
}

// `load*_lane` replaces one lane of `value` with an element read from
// memory; `store*_lane` writes one lane of `value` back to memory. `lane`
// is a validated lane index, so the byte range is always in bounds.
//# load8_lane
fn @NAME(&self, addr: @ADDR, offset: @OFF, value: u128, lane: usize) -> u128 {
    let a = @EA;
    let mut b = value.to_le_bytes();
    b[lane] = @R[a];
    u128::from_le_bytes(b)
}

//# load16_lane
fn @NAME(&self, addr: @ADDR, offset: @OFF, value: u128, lane: usize) -> u128 {
    let a = @EA;
    let o = lane * 2;
    let mut b = value.to_le_bytes();
    b[o..o + 2].copy_from_slice(&@R[a..a + 2]);
    u128::from_le_bytes(b)
}

//# load32_lane
fn @NAME(&self, addr: @ADDR, offset: @OFF, value: u128, lane: usize) -> u128 {
    let a = @EA;
    let o = lane * 4;
    let mut b = value.to_le_bytes();
    b[o..o + 4].copy_from_slice(&@R[a..a + 4]);
    u128::from_le_bytes(b)
}

//# load64_lane
fn @NAME(&self, addr: @ADDR, offset: @OFF, value: u128, lane: usize) -> u128 {
    let a = @EA;
    let o = lane * 8;
    let mut b = value.to_le_bytes();
    b[o..o + 8].copy_from_slice(&@R[a..a + 8]);
    u128::from_le_bytes(b)
}

//# store8_lane
fn @NAME(&mut self, addr: @ADDR, offset: @OFF, value: u128, lane: usize) {
    let a = @EA;
    @W[a] = value.to_le_bytes()[lane];
}

//# store16_lane
fn @NAME(&mut self, addr: @ADDR, offset: @OFF, value: u128, lane: usize) {
    let a = @EA;
    let o = lane * 2;
    @W[a..a + 2].copy_from_slice(&value.to_le_bytes()[o..o + 2]);
}

//# store32_lane
fn @NAME(&mut self, addr: @ADDR, offset: @OFF, value: u128, lane: usize) {
    let a = @EA;
    let o = lane * 4;
    @W[a..a + 4].copy_from_slice(&value.to_le_bytes()[o..o + 4]);
}

//# store64_lane
fn @NAME(&mut self, addr: @ADDR, offset: @OFF, value: u128, lane: usize) {
    let a = @EA;
    let o = lane * 8;
    @W[a..a + 8].copy_from_slice(&value.to_le_bytes()[o..o + 8]);
}

// Bulk operations. An out-of-bounds range panics on the slice access or
// `copy_within` (a wasm trap); `copy_within` is memmove, so overlapping
// source and destination copy correctly.
//# memory_fill
fn @NAME(&mut self, dest: u32, val: i32, len: u32) {
    let d = dest as usize;
    @W[d..d + len as usize].fill(val as u8);
}

//# memory_copy
fn @NAME(&mut self, dest: u32, src: u32, len: u32) {
    let s = src as usize;
    let d = dest as usize;
    @W.copy_within(s..s + len as usize, d);
}

//# table_copy
fn @NAME(&mut self, dest: u32, src: u32, len: u32) {
    let s = src as usize;
    let d = dest as usize;
    self.table_mut().copy_within(s..s + len as usize, d);
}

//# table_fill
fn @NAME(&mut self, dest: u32, val: u32, len: u32) {
    let d = dest as usize;
    self.table_mut()[d..d + len as usize].fill(val);
}
