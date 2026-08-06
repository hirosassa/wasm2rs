use super::Helper;

/// The accessor pair `(mem(), mem_mut())` naming linear memory `mem`: memory 0
/// keeps the historic `mem`/`mem_mut` names (so index-0 code and WASI are
/// byte-for-byte unchanged), while memory `i > 0` uses `mem{i}`/`mem{i}_mut`.
pub(super) fn mem_accessor(mem: u32) -> (String, String) {
    if mem == 0 {
        ("mem".to_string(), "mem_mut".to_string())
    } else {
        (format!("mem{mem}"), format!("mem{mem}_mut"))
    }
}

/// The instance method name of `helper` for linear memory `mem`: memory 0 keeps
/// the historic name (`r32`, `memory_grow`, …); memory `i > 0` appends `_m{i}`
/// so each memory owns a distinct set of helper methods (`r32_m1`, …).
pub(super) fn helper_method_name(helper: Helper, mem: u32) -> String {
    let base = helper_name(helper);
    if mem == 0 {
        base.to_string()
    } else {
        format!("{base}_m{mem}")
    }
}

pub(super) fn helper_name(helper: Helper) -> &'static str {
    match helper {
        // The scalar load/store helpers are the hottest methods in generated
        // code, so their names are terse: `r*` reads (loads), `w*` writes
        // (stores). The prefix cannot collide with locals (`l`), temporaries
        // (`v`), globals (`g`), or functions (`func`).
        Helper::LoadI32 => "r32",
        Helper::Load8U => "r8u",
        Helper::Load8S => "r8s",
        Helper::Load16U => "r16u",
        Helper::Load16S => "r16s",
        Helper::LoadI64 => "r64",
        Helper::LoadF32 => "rf32",
        Helper::LoadF64 => "rf64",
        Helper::Load8UI64 => "r8u64",
        Helper::Load8SI64 => "r8s64",
        Helper::Load16UI64 => "r16u64",
        Helper::Load16SI64 => "r16s64",
        Helper::Load32UI64 => "r32u64",
        Helper::Load32SI64 => "r32s64",
        Helper::StoreI32 => "w32",
        Helper::Store8 => "w8",
        Helper::Store16 => "w16",
        Helper::StoreI64 => "w64",
        Helper::StoreF32 => "wf32",
        Helper::StoreF64 => "wf64",
        Helper::Store8I64 => "w8_64",
        Helper::Store16I64 => "w16_64",
        Helper::Store32I64 => "w32_64",
        Helper::LoadV128 => "rv128",
        Helper::StoreV128 => "wv128",
        Helper::Load8Splat => "load8_splat",
        Helper::Load16Splat => "load16_splat",
        Helper::Load32Splat => "load32_splat",
        Helper::Load64Splat => "load64_splat",
        Helper::Load32Zero => "load32_zero",
        Helper::Load64Zero => "load64_zero",
        Helper::Load8x8S => "load8x8_s",
        Helper::Load8x8U => "load8x8_u",
        Helper::Load16x4S => "load16x4_s",
        Helper::Load16x4U => "load16x4_u",
        Helper::Load32x2S => "load32x2_s",
        Helper::Load32x2U => "load32x2_u",
        Helper::Load8Lane => "load8_lane",
        Helper::Load16Lane => "load16_lane",
        Helper::Load32Lane => "load32_lane",
        Helper::Load64Lane => "load64_lane",
        Helper::Store8Lane => "store8_lane",
        Helper::Store16Lane => "store16_lane",
        Helper::Store32Lane => "store32_lane",
        Helper::Store64Lane => "store64_lane",
        Helper::Grow => "memory_grow",
        Helper::MemoryFill => "memory_fill",
        Helper::MemoryCopy => "memory_copy",
        Helper::TableCopy => "table_copy",
        Helper::TableFill => "table_fill",
    }
}

/// All memory helpers, in a deterministic emission order.
pub(super) const HELPER_ORDER: [Helper; 50] = [
    Helper::LoadI32,
    Helper::Load8U,
    Helper::Load8S,
    Helper::Load16U,
    Helper::Load16S,
    Helper::LoadI64,
    Helper::LoadF32,
    Helper::LoadF64,
    Helper::Load8UI64,
    Helper::Load8SI64,
    Helper::Load16UI64,
    Helper::Load16SI64,
    Helper::Load32UI64,
    Helper::Load32SI64,
    Helper::StoreI32,
    Helper::Store8,
    Helper::Store16,
    Helper::StoreI64,
    Helper::StoreF32,
    Helper::StoreF64,
    Helper::Store8I64,
    Helper::Store16I64,
    Helper::Store32I64,
    Helper::LoadV128,
    Helper::StoreV128,
    Helper::Load8Splat,
    Helper::Load16Splat,
    Helper::Load32Splat,
    Helper::Load64Splat,
    Helper::Load32Zero,
    Helper::Load64Zero,
    Helper::Load8x8S,
    Helper::Load8x8U,
    Helper::Load16x4S,
    Helper::Load16x4U,
    Helper::Load32x2S,
    Helper::Load32x2U,
    Helper::Load8Lane,
    Helper::Load16Lane,
    Helper::Load32Lane,
    Helper::Load64Lane,
    Helper::Store8Lane,
    Helper::Store16Lane,
    Helper::Store32Lane,
    Helper::Store64Lane,
    Helper::Grow,
    Helper::MemoryFill,
    Helper::MemoryCopy,
    Helper::TableCopy,
    Helper::TableFill,
];

/// The source lines of one memory helper method for linear memory `mem`
/// (bounds-checked via indexing, so an out-of-range access panics — mirroring a
/// wasm trap).
///
/// For memory 0 the emitted text is byte-for-byte the historic single-memory
/// form (method name and `self.mem()`/`self.mem_mut()` body). For memory
/// `i > 0` the method name gains an `_m{i}` suffix and the body borrows
/// `self.mem{i}()`/`self.mem{i}_mut()`, so each memory owns an independent
/// helper set.
pub(super) fn helper_lines(helper: Helper, mem: u32) -> Vec<String> {
    let base = base_helper_lines(helper);
    if mem == 0 {
        return base;
    }
    specialise_helper_lines(helper, mem, base)
}

/// Specialise the memory-0 template to memory `i > 0`: rename the method and
/// retarget each `self.mem()`/`self.mem_mut()` to the `mem{i}` accessors.
fn specialise_helper_lines(helper: Helper, mem: u32, base: Vec<String>) -> Vec<String> {
    // Specialise the memory-0 template to memory `i`: rename the method (the
    // helper name appears once, right after `fn `, in the signature line) and
    // retarget every memory accessor to the `mem{i}` variants. `table_*` helpers
    // touch no memory, so they are never emitted for `i > 0`.
    let name = helper_name(helper);
    let (get, get_mut) = mem_accessor(mem);
    base.into_iter()
        .map(|line| {
            line.replacen(&format!("fn {name}("), &format!("fn {name}_m{mem}("), 1)
                .replace("self.mem_mut()", &format!("self.{get_mut}()"))
                .replace("self.mem()", &format!("self.{get}()"))
        })
        .collect()
}

/// The lines of one memory helper method for a `shared` module (single defined
/// memory 0 backed by a `SharedMemory`). The historic body references
/// `self.mem()` / `self.mem_mut()`; here the whole body must lock the shared
/// bytes exactly once (a per-access re-lock would deadlock the non-reentrant
/// `Mutex`), so we bind one guard right after the signature and replace every
/// `self.mem()` / `self.mem_mut()` with it. A body that writes (`mem_mut()`
/// appears) binds a `mut` guard; a read-only body binds an immutable one. The
/// method name and signature are unchanged (`fn r32(&self, ...)`, …); a store
/// helper keeps its `&mut self` even though `bytes()` needs only `&self`.
/// `table_*` helpers touch no memory, so they are emitted verbatim.
pub(super) fn shared_helper_lines(helper: Helper) -> Vec<String> {
    let base = base_helper_lines(helper);
    if matches!(helper, Helper::TableCopy | Helper::TableFill) {
        return base;
    }
    let writes = base.iter().any(|line| line.contains("self.mem_mut()"));
    let guard = if writes {
        "    let mut __m = self.memory.bytes();"
    } else {
        "    let __m = self.memory.bytes();"
    };
    let mut out = Vec::with_capacity(base.len() + 1);
    for (i, line) in base.into_iter().enumerate() {
        // The signature line is first; the guard is bound immediately after it,
        // before any memory access, so the lock is held for the whole body.
        let line = line
            .replace("self.mem_mut()", "__m")
            .replace("self.mem()", "__m");
        out.push(line);
        if i == 0 {
            out.push(guard.to_string());
        }
    }
    out
}

/// The memory-0 template lines of one memory helper method.
fn base_helper_lines(helper: Helper) -> Vec<String> {
    let owned = |lines: &[&str]| lines.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    match helper {
        Helper::LoadI32 => owned(&[
            "fn r32(&self, addr: i32, offset: u32) -> i32 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    i32::from_le_bytes(self.mem()[a..a + 4].try_into().unwrap())",
            "}",
        ]),
        Helper::Load8U => owned(&[
            "fn r8u(&self, addr: i32, offset: u32) -> i32 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    self.mem()[a] as i32",
            "}",
        ]),
        Helper::Load8S => owned(&[
            "fn r8s(&self, addr: i32, offset: u32) -> i32 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    self.mem()[a] as i8 as i32",
            "}",
        ]),
        Helper::Load16U => owned(&[
            "fn r16u(&self, addr: i32, offset: u32) -> i32 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    u16::from_le_bytes(self.mem()[a..a + 2].try_into().unwrap()) as i32",
            "}",
        ]),
        Helper::Load16S => owned(&[
            "fn r16s(&self, addr: i32, offset: u32) -> i32 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    i16::from_le_bytes(self.mem()[a..a + 2].try_into().unwrap()) as i32",
            "}",
        ]),
        Helper::StoreI32 => owned(&[
            "fn w32(&mut self, addr: i32, offset: u32, value: i32) {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    self.mem_mut()[a..a + 4].copy_from_slice(&value.to_le_bytes());",
            "}",
        ]),
        Helper::Store8 => owned(&[
            "fn w8(&mut self, addr: i32, offset: u32, value: i32) {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    self.mem_mut()[a] = value as u8;",
            "}",
        ]),
        Helper::Store16 => owned(&[
            "fn w16(&mut self, addr: i32, offset: u32, value: i32) {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    self.mem_mut()[a..a + 2].copy_from_slice(&(value as u16).to_le_bytes());",
            "}",
        ]),
        Helper::LoadI64 => owned(&[
            "fn r64(&self, addr: i32, offset: u32) -> i64 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    i64::from_le_bytes(self.mem()[a..a + 8].try_into().unwrap())",
            "}",
        ]),
        Helper::LoadF32 => owned(&[
            "fn rf32(&self, addr: i32, offset: u32) -> f32 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    f32::from_le_bytes(self.mem()[a..a + 4].try_into().unwrap())",
            "}",
        ]),
        Helper::LoadF64 => owned(&[
            "fn rf64(&self, addr: i32, offset: u32) -> f64 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    f64::from_le_bytes(self.mem()[a..a + 8].try_into().unwrap())",
            "}",
        ]),
        Helper::Load8UI64 => owned(&[
            "fn r8u64(&self, addr: i32, offset: u32) -> i64 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    self.mem()[a] as i64",
            "}",
        ]),
        Helper::Load8SI64 => owned(&[
            "fn r8s64(&self, addr: i32, offset: u32) -> i64 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    self.mem()[a] as i8 as i64",
            "}",
        ]),
        Helper::Load16UI64 => owned(&[
            "fn r16u64(&self, addr: i32, offset: u32) -> i64 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    u16::from_le_bytes(self.mem()[a..a + 2].try_into().unwrap()) as i64",
            "}",
        ]),
        Helper::Load16SI64 => owned(&[
            "fn r16s64(&self, addr: i32, offset: u32) -> i64 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    i16::from_le_bytes(self.mem()[a..a + 2].try_into().unwrap()) as i64",
            "}",
        ]),
        Helper::Load32UI64 => owned(&[
            "fn r32u64(&self, addr: i32, offset: u32) -> i64 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    u32::from_le_bytes(self.mem()[a..a + 4].try_into().unwrap()) as i64",
            "}",
        ]),
        Helper::Load32SI64 => owned(&[
            "fn r32s64(&self, addr: i32, offset: u32) -> i64 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    i32::from_le_bytes(self.mem()[a..a + 4].try_into().unwrap()) as i64",
            "}",
        ]),
        Helper::StoreI64 => owned(&[
            "fn w64(&mut self, addr: i32, offset: u32, value: i64) {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    self.mem_mut()[a..a + 8].copy_from_slice(&value.to_le_bytes());",
            "}",
        ]),
        Helper::StoreF32 => owned(&[
            "fn wf32(&mut self, addr: i32, offset: u32, value: f32) {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    self.mem_mut()[a..a + 4].copy_from_slice(&value.to_le_bytes());",
            "}",
        ]),
        Helper::StoreF64 => owned(&[
            "fn wf64(&mut self, addr: i32, offset: u32, value: f64) {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    self.mem_mut()[a..a + 8].copy_from_slice(&value.to_le_bytes());",
            "}",
        ]),
        Helper::Store8I64 => owned(&[
            "fn w8_64(&mut self, addr: i32, offset: u32, value: i64) {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    self.mem_mut()[a] = value as u8;",
            "}",
        ]),
        Helper::Store16I64 => owned(&[
            "fn w16_64(&mut self, addr: i32, offset: u32, value: i64) {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    self.mem_mut()[a..a + 2].copy_from_slice(&(value as u16).to_le_bytes());",
            "}",
        ]),
        Helper::Store32I64 => owned(&[
            "fn w32_64(&mut self, addr: i32, offset: u32, value: i64) {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    self.mem_mut()[a..a + 4].copy_from_slice(&(value as u32).to_le_bytes());",
            "}",
        ]),
        // A v128 load/store moves 16 bytes to/from memory as a little-endian
        // `u128`; the slice access is bounds-checked, so an out-of-range access
        // panics (a wasm trap).
        Helper::LoadV128 => owned(&[
            "fn rv128(&self, addr: i32, offset: u32) -> u128 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    let mut b = [0u8; 16];",
            "    b.copy_from_slice(&self.mem()[a..a + 16]);",
            "    u128::from_le_bytes(b)",
            "}",
        ]),
        Helper::StoreV128 => owned(&[
            "fn wv128(&mut self, addr: i32, offset: u32, value: u128) {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    self.mem_mut()[a..a + 16].copy_from_slice(&value.to_le_bytes());",
            "}",
        ]),
        // `load*_splat` reads one element and broadcasts it to every lane;
        // `load*_zero` reads one into the low lane and zeroes the rest. Each
        // access is bounds-checked, so an out-of-range read traps.
        Helper::Load8Splat => owned(&[
            "fn load8_splat(&self, addr: i32, offset: u32) -> u128 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    u128::from_le_bytes([self.mem()[a]; 16])",
            "}",
        ]),
        Helper::Load16Splat => owned(&[
            "fn load16_splat(&self, addr: i32, offset: u32) -> u128 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    let lane = [self.mem()[a], self.mem()[a + 1]];",
            "    let mut b = [0u8; 16];",
            "    let mut i = 0;",
            "    while i < 16 {",
            "        b[i..i + 2].copy_from_slice(&lane);",
            "        i += 2;",
            "    }",
            "    u128::from_le_bytes(b)",
            "}",
        ]),
        Helper::Load32Splat => owned(&[
            "fn load32_splat(&self, addr: i32, offset: u32) -> u128 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    let lane = [self.mem()[a], self.mem()[a + 1], self.mem()[a + 2], self.mem()[a + 3]];",
            "    let mut b = [0u8; 16];",
            "    let mut i = 0;",
            "    while i < 16 {",
            "        b[i..i + 4].copy_from_slice(&lane);",
            "        i += 4;",
            "    }",
            "    u128::from_le_bytes(b)",
            "}",
        ]),
        Helper::Load64Splat => owned(&[
            "fn load64_splat(&self, addr: i32, offset: u32) -> u128 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    let mut b = [0u8; 16];",
            "    b[0..8].copy_from_slice(&self.mem()[a..a + 8]);",
            "    b.copy_within(0..8, 8);",
            "    u128::from_le_bytes(b)",
            "}",
        ]),
        Helper::Load32Zero => owned(&[
            "fn load32_zero(&self, addr: i32, offset: u32) -> u128 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    let mut b = [0u8; 16];",
            "    b[0..4].copy_from_slice(&self.mem()[a..a + 4]);",
            "    u128::from_le_bytes(b)",
            "}",
        ]),
        Helper::Load64Zero => owned(&[
            "fn load64_zero(&self, addr: i32, offset: u32) -> u128 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    let mut b = [0u8; 16];",
            "    b[0..8].copy_from_slice(&self.mem()[a..a + 8]);",
            "    u128::from_le_bytes(b)",
            "}",
        ]),
        // `load{8x8,16x4,32x2}_{s,u}` read eight bytes and widen each source lane
        // to a double-width lane: `_s` sign-extends, `_u` zero-extends. The eight
        // bytes are bounds-checked, so an out-of-range read traps.
        Helper::Load8x8S => owned(&[
            "fn load8x8_s(&self, addr: i32, offset: u32) -> u128 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    let mut r = [0u8; 16];",
            "    let mut i = 0;",
            "    while i < 8 {",
            "        let x = self.mem()[a + i] as i8 as i16;",
            "        r[i * 2..i * 2 + 2].copy_from_slice(&x.to_le_bytes());",
            "        i += 1;",
            "    }",
            "    u128::from_le_bytes(r)",
            "}",
        ]),
        Helper::Load8x8U => owned(&[
            "fn load8x8_u(&self, addr: i32, offset: u32) -> u128 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    let mut r = [0u8; 16];",
            "    let mut i = 0;",
            "    while i < 8 {",
            "        let x = self.mem()[a + i] as u16;",
            "        r[i * 2..i * 2 + 2].copy_from_slice(&x.to_le_bytes());",
            "        i += 1;",
            "    }",
            "    u128::from_le_bytes(r)",
            "}",
        ]),
        Helper::Load16x4S => owned(&[
            "fn load16x4_s(&self, addr: i32, offset: u32) -> u128 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    let mut r = [0u8; 16];",
            "    let mut i = 0;",
            "    while i < 4 {",
            "        let s = a + i * 2;",
            "        let x = i16::from_le_bytes([self.mem()[s], self.mem()[s + 1]]) as i32;",
            "        r[i * 4..i * 4 + 4].copy_from_slice(&x.to_le_bytes());",
            "        i += 1;",
            "    }",
            "    u128::from_le_bytes(r)",
            "}",
        ]),
        Helper::Load16x4U => owned(&[
            "fn load16x4_u(&self, addr: i32, offset: u32) -> u128 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    let mut r = [0u8; 16];",
            "    let mut i = 0;",
            "    while i < 4 {",
            "        let s = a + i * 2;",
            "        let x = u16::from_le_bytes([self.mem()[s], self.mem()[s + 1]]) as u32;",
            "        r[i * 4..i * 4 + 4].copy_from_slice(&x.to_le_bytes());",
            "        i += 1;",
            "    }",
            "    u128::from_le_bytes(r)",
            "}",
        ]),
        Helper::Load32x2S => owned(&[
            "fn load32x2_s(&self, addr: i32, offset: u32) -> u128 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    let mut r = [0u8; 16];",
            "    let mut i = 0;",
            "    while i < 2 {",
            "        let s = a + i * 4;",
            "        let x = i32::from_le_bytes([self.mem()[s], self.mem()[s + 1], self.mem()[s + 2], self.mem()[s + 3]]) as i64;",
            "        r[i * 8..i * 8 + 8].copy_from_slice(&x.to_le_bytes());",
            "        i += 1;",
            "    }",
            "    u128::from_le_bytes(r)",
            "}",
        ]),
        Helper::Load32x2U => owned(&[
            "fn load32x2_u(&self, addr: i32, offset: u32) -> u128 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    let mut r = [0u8; 16];",
            "    let mut i = 0;",
            "    while i < 2 {",
            "        let s = a + i * 4;",
            "        let x = u32::from_le_bytes([self.mem()[s], self.mem()[s + 1], self.mem()[s + 2], self.mem()[s + 3]]) as u64;",
            "        r[i * 8..i * 8 + 8].copy_from_slice(&x.to_le_bytes());",
            "        i += 1;",
            "    }",
            "    u128::from_le_bytes(r)",
            "}",
        ]),
        // `load*_lane` replaces one lane of `value` with an element read from
        // memory; `store*_lane` writes one lane of `value` back to memory. `lane`
        // is a validated lane index, so the byte range is always in bounds.
        Helper::Load8Lane => owned(&[
            "fn load8_lane(&self, addr: i32, offset: u32, value: u128, lane: usize) -> u128 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    let mut b = value.to_le_bytes();",
            "    b[lane] = self.mem()[a];",
            "    u128::from_le_bytes(b)",
            "}",
        ]),
        Helper::Load16Lane => owned(&[
            "fn load16_lane(&self, addr: i32, offset: u32, value: u128, lane: usize) -> u128 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    let o = lane * 2;",
            "    let mut b = value.to_le_bytes();",
            "    b[o..o + 2].copy_from_slice(&self.mem()[a..a + 2]);",
            "    u128::from_le_bytes(b)",
            "}",
        ]),
        Helper::Load32Lane => owned(&[
            "fn load32_lane(&self, addr: i32, offset: u32, value: u128, lane: usize) -> u128 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    let o = lane * 4;",
            "    let mut b = value.to_le_bytes();",
            "    b[o..o + 4].copy_from_slice(&self.mem()[a..a + 4]);",
            "    u128::from_le_bytes(b)",
            "}",
        ]),
        Helper::Load64Lane => owned(&[
            "fn load64_lane(&self, addr: i32, offset: u32, value: u128, lane: usize) -> u128 {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    let o = lane * 8;",
            "    let mut b = value.to_le_bytes();",
            "    b[o..o + 8].copy_from_slice(&self.mem()[a..a + 8]);",
            "    u128::from_le_bytes(b)",
            "}",
        ]),
        Helper::Store8Lane => owned(&[
            "fn store8_lane(&mut self, addr: i32, offset: u32, value: u128, lane: usize) {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    self.mem_mut()[a] = value.to_le_bytes()[lane];",
            "}",
        ]),
        Helper::Store16Lane => owned(&[
            "fn store16_lane(&mut self, addr: i32, offset: u32, value: u128, lane: usize) {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    let o = lane * 2;",
            "    self.mem_mut()[a..a + 2].copy_from_slice(&value.to_le_bytes()[o..o + 2]);",
            "}",
        ]),
        Helper::Store32Lane => owned(&[
            "fn store32_lane(&mut self, addr: i32, offset: u32, value: u128, lane: usize) {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    let o = lane * 4;",
            "    self.mem_mut()[a..a + 4].copy_from_slice(&value.to_le_bytes()[o..o + 4]);",
            "}",
        ]),
        Helper::Store64Lane => owned(&[
            "fn store64_lane(&mut self, addr: i32, offset: u32, value: u128, lane: usize) {",
            "    let a = addr as u32 as usize + offset as usize;",
            "    let o = lane * 8;",
            "    self.mem_mut()[a..a + 8].copy_from_slice(&value.to_le_bytes()[o..o + 8]);",
            "}",
        ]),
        // `delta` is an unsigned page count. Growth past the wasm32 limit of
        // 65536 pages (4 GiB) fails, returning -1 as the wasm spec requires;
        // the declared maximum is not tracked, so only that hard cap applies.
        Helper::Grow => owned(&[
            "fn memory_grow(&mut self, delta: i32) -> i32 {",
            "    let old_pages = (self.mem().len() / 65536) as u64;",
            "    let new_pages = old_pages + (delta as u32 as u64);",
            "    if new_pages > 65536 {",
            "        return -1;",
            "    }",
            "    self.mem_mut().resize((new_pages as usize) * 65536, 0);",
            "    old_pages as i32",
            "}",
        ]),
        // Bulk operations. An out-of-bounds range panics on the slice access or
        // `copy_within` (a wasm trap); `copy_within` is memmove, so overlapping
        // source and destination copy correctly.
        Helper::MemoryFill => owned(&[
            "fn memory_fill(&mut self, dest: u32, val: i32, len: u32) {",
            "    let d = dest as usize;",
            "    self.mem_mut()[d..d + len as usize].fill(val as u8);",
            "}",
        ]),
        Helper::MemoryCopy => owned(&[
            "fn memory_copy(&mut self, dest: u32, src: u32, len: u32) {",
            "    let s = src as usize;",
            "    let d = dest as usize;",
            "    self.mem_mut().copy_within(s..s + len as usize, d);",
            "}",
        ]),
        Helper::TableCopy => owned(&[
            "fn table_copy(&mut self, dest: u32, src: u32, len: u32) {",
            "    let s = src as usize;",
            "    let d = dest as usize;",
            "    self.table_mut().copy_within(s..s + len as usize, d);",
            "}",
        ]),
        Helper::TableFill => owned(&[
            "fn table_fill(&mut self, dest: u32, val: u32, len: u32) {",
            "    let d = dest as usize;",
            "    self.table_mut()[d..d + len as usize].fill(val);",
            "}",
        ]),
    }
}
