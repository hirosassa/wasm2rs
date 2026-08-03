use super::Helper;

pub(super) fn helper_name(helper: Helper) -> &'static str {
    match helper {
        Helper::LoadI32 => "load_i32",
        Helper::Load8U => "load8_u",
        Helper::Load8S => "load8_s",
        Helper::Load16U => "load16_u",
        Helper::Load16S => "load16_s",
        Helper::LoadI64 => "load_i64",
        Helper::LoadF32 => "load_f32",
        Helper::LoadF64 => "load_f64",
        Helper::Load8UI64 => "load8_u_i64",
        Helper::Load8SI64 => "load8_s_i64",
        Helper::Load16UI64 => "load16_u_i64",
        Helper::Load16SI64 => "load16_s_i64",
        Helper::Load32UI64 => "load32_u_i64",
        Helper::Load32SI64 => "load32_s_i64",
        Helper::StoreI32 => "store_i32",
        Helper::Store8 => "store8",
        Helper::Store16 => "store16",
        Helper::StoreI64 => "store_i64",
        Helper::StoreF32 => "store_f32",
        Helper::StoreF64 => "store_f64",
        Helper::Store8I64 => "store8_i64",
        Helper::Store16I64 => "store16_i64",
        Helper::Store32I64 => "store32_i64",
        Helper::Grow => "memory_grow",
        Helper::MemoryFill => "memory_fill",
        Helper::MemoryCopy => "memory_copy",
        Helper::TableCopy => "table_copy",
        Helper::TableFill => "table_fill",
    }
}

/// All memory helpers, in a deterministic emission order.
pub(super) const HELPER_ORDER: [Helper; 28] = [
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
    Helper::Grow,
    Helper::MemoryFill,
    Helper::MemoryCopy,
    Helper::TableCopy,
    Helper::TableFill,
];

/// The source lines of one memory helper method (bounds-checked via indexing,
/// so an out-of-range access panics — mirroring a wasm trap).
pub(super) fn helper_lines(helper: Helper) -> Vec<String> {
    let owned = |lines: &[&str]| lines.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    match helper {
        Helper::LoadI32 => owned(&[
            "fn load_i32(&self, addr: u32, offset: u32) -> i32 {",
            "    let a = addr as usize + offset as usize;",
            "    i32::from_le_bytes([self.mem()[a], self.mem()[a + 1], self.mem()[a + 2], self.mem()[a + 3]])",
            "}",
        ]),
        Helper::Load8U => owned(&[
            "fn load8_u(&self, addr: u32, offset: u32) -> i32 {",
            "    let a = addr as usize + offset as usize;",
            "    self.mem()[a] as i32",
            "}",
        ]),
        Helper::Load8S => owned(&[
            "fn load8_s(&self, addr: u32, offset: u32) -> i32 {",
            "    let a = addr as usize + offset as usize;",
            "    self.mem()[a] as i8 as i32",
            "}",
        ]),
        Helper::Load16U => owned(&[
            "fn load16_u(&self, addr: u32, offset: u32) -> i32 {",
            "    let a = addr as usize + offset as usize;",
            "    u16::from_le_bytes([self.mem()[a], self.mem()[a + 1]]) as i32",
            "}",
        ]),
        Helper::Load16S => owned(&[
            "fn load16_s(&self, addr: u32, offset: u32) -> i32 {",
            "    let a = addr as usize + offset as usize;",
            "    i16::from_le_bytes([self.mem()[a], self.mem()[a + 1]]) as i32",
            "}",
        ]),
        Helper::StoreI32 => owned(&[
            "fn store_i32(&mut self, addr: u32, offset: u32, value: i32) {",
            "    let a = addr as usize + offset as usize;",
            "    self.mem_mut()[a..a + 4].copy_from_slice(&value.to_le_bytes());",
            "}",
        ]),
        Helper::Store8 => owned(&[
            "fn store8(&mut self, addr: u32, offset: u32, value: i32) {",
            "    let a = addr as usize + offset as usize;",
            "    self.mem_mut()[a] = value as u8;",
            "}",
        ]),
        Helper::Store16 => owned(&[
            "fn store16(&mut self, addr: u32, offset: u32, value: i32) {",
            "    let a = addr as usize + offset as usize;",
            "    self.mem_mut()[a..a + 2].copy_from_slice(&(value as u16).to_le_bytes());",
            "}",
        ]),
        Helper::LoadI64 => owned(&[
            "fn load_i64(&self, addr: u32, offset: u32) -> i64 {",
            "    let a = addr as usize + offset as usize;",
            "    i64::from_le_bytes([self.mem()[a], self.mem()[a + 1], self.mem()[a + 2], self.mem()[a + 3], self.mem()[a + 4], self.mem()[a + 5], self.mem()[a + 6], self.mem()[a + 7]])",
            "}",
        ]),
        Helper::LoadF32 => owned(&[
            "fn load_f32(&self, addr: u32, offset: u32) -> f32 {",
            "    let a = addr as usize + offset as usize;",
            "    f32::from_le_bytes([self.mem()[a], self.mem()[a + 1], self.mem()[a + 2], self.mem()[a + 3]])",
            "}",
        ]),
        Helper::LoadF64 => owned(&[
            "fn load_f64(&self, addr: u32, offset: u32) -> f64 {",
            "    let a = addr as usize + offset as usize;",
            "    f64::from_le_bytes([self.mem()[a], self.mem()[a + 1], self.mem()[a + 2], self.mem()[a + 3], self.mem()[a + 4], self.mem()[a + 5], self.mem()[a + 6], self.mem()[a + 7]])",
            "}",
        ]),
        Helper::Load8UI64 => owned(&[
            "fn load8_u_i64(&self, addr: u32, offset: u32) -> i64 {",
            "    let a = addr as usize + offset as usize;",
            "    self.mem()[a] as i64",
            "}",
        ]),
        Helper::Load8SI64 => owned(&[
            "fn load8_s_i64(&self, addr: u32, offset: u32) -> i64 {",
            "    let a = addr as usize + offset as usize;",
            "    self.mem()[a] as i8 as i64",
            "}",
        ]),
        Helper::Load16UI64 => owned(&[
            "fn load16_u_i64(&self, addr: u32, offset: u32) -> i64 {",
            "    let a = addr as usize + offset as usize;",
            "    u16::from_le_bytes([self.mem()[a], self.mem()[a + 1]]) as i64",
            "}",
        ]),
        Helper::Load16SI64 => owned(&[
            "fn load16_s_i64(&self, addr: u32, offset: u32) -> i64 {",
            "    let a = addr as usize + offset as usize;",
            "    i16::from_le_bytes([self.mem()[a], self.mem()[a + 1]]) as i64",
            "}",
        ]),
        Helper::Load32UI64 => owned(&[
            "fn load32_u_i64(&self, addr: u32, offset: u32) -> i64 {",
            "    let a = addr as usize + offset as usize;",
            "    u32::from_le_bytes([self.mem()[a], self.mem()[a + 1], self.mem()[a + 2], self.mem()[a + 3]]) as i64",
            "}",
        ]),
        Helper::Load32SI64 => owned(&[
            "fn load32_s_i64(&self, addr: u32, offset: u32) -> i64 {",
            "    let a = addr as usize + offset as usize;",
            "    i32::from_le_bytes([self.mem()[a], self.mem()[a + 1], self.mem()[a + 2], self.mem()[a + 3]]) as i64",
            "}",
        ]),
        Helper::StoreI64 => owned(&[
            "fn store_i64(&mut self, addr: u32, offset: u32, value: i64) {",
            "    let a = addr as usize + offset as usize;",
            "    self.mem_mut()[a..a + 8].copy_from_slice(&value.to_le_bytes());",
            "}",
        ]),
        Helper::StoreF32 => owned(&[
            "fn store_f32(&mut self, addr: u32, offset: u32, value: f32) {",
            "    let a = addr as usize + offset as usize;",
            "    self.mem_mut()[a..a + 4].copy_from_slice(&value.to_le_bytes());",
            "}",
        ]),
        Helper::StoreF64 => owned(&[
            "fn store_f64(&mut self, addr: u32, offset: u32, value: f64) {",
            "    let a = addr as usize + offset as usize;",
            "    self.mem_mut()[a..a + 8].copy_from_slice(&value.to_le_bytes());",
            "}",
        ]),
        Helper::Store8I64 => owned(&[
            "fn store8_i64(&mut self, addr: u32, offset: u32, value: i64) {",
            "    let a = addr as usize + offset as usize;",
            "    self.mem_mut()[a] = value as u8;",
            "}",
        ]),
        Helper::Store16I64 => owned(&[
            "fn store16_i64(&mut self, addr: u32, offset: u32, value: i64) {",
            "    let a = addr as usize + offset as usize;",
            "    self.mem_mut()[a..a + 2].copy_from_slice(&(value as u16).to_le_bytes());",
            "}",
        ]),
        Helper::Store32I64 => owned(&[
            "fn store32_i64(&mut self, addr: u32, offset: u32, value: i64) {",
            "    let a = addr as usize + offset as usize;",
            "    self.mem_mut()[a..a + 4].copy_from_slice(&(value as u32).to_le_bytes());",
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
