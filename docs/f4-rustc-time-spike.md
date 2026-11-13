# F4-0: where `rustc` time goes on large components (2026-11-13, wasm2rs 0.4.0)

Spike for F4 of [component-toolchain-survey.md](component-toolchain-survey.md), issue #316 and
[f2-dynamic-linking-design.md](f2-dynamic-linking-design.md). **Docs only; no product code
changed.** Tags: **[verified]** = measured here, **[inferred]** = reasoned, not measured.

## Result in one paragraph

The cost is **not** a giant function, not LLVM, and not the opt level. It is a `rustc` early-lint
step (`UnusedParens` -> `SourceMap::span_to_next_source`) that, for every redundant
parenthesis around a method argument / assigned value / block tail, **copies the rest of the
source file** to look at the next character. On one 124 MB file that is quadratic, and it
accounts for about 90% of the time (system time equals user time, [inferred] from the huge
allocations). The generated code is full of such parentheses (`self.w32(((a) + (b)), v)`), and
`#[allow(unused_parens)]` does **not** switch the work off. Two independent fixes both bring the
JS component from **587 s to about 61 s** (`program.rs`; 568 s -> 62 s for the equivalent core module) [verified]: (A) do not emit those parentheses,
(B) emit several smaller source files. **Recommendation: A** (small, no layout change), keep B
as a follow-up for the memory floor.

## Setup

- `rustc 1.99.1`, aarch64 macOS, 16 cores, 64 GB. One `rustc` at a time; 30 min cap per run
  (never reached). Times are wall seconds, `-C opt-level=1` unless stated, `--crate-type lib`.
- Inputs: the 124 MB / 1.89 M-line JS `program.rs` (12,722 functions), the Go component
  (`--component`, 29 MB / 458 K lines), and the JS core module (`--extract-core` module0,
  transpiled with `--named-imports --named-exports --reentrant-imports`: a 124 MB single file,
  or the split layout of `funcs_per_file` / `max_bytes_per_file`).
- Python: only partly covered, see "Not measured".

## 1. Which code is slow? (step 1) [verified]

Largest functions in the JS `program.rs` (lines, `match` arms, deepest indentation level):

| function | lines | arms | depth |
|---|---|---|---|
| `func11932` | 47,142 | 6,728 | 5 |
| `func11588` | 46,319 | 6,492 | 20 |
| `func11972` | 41,603 | 6,134 | 7 |
| `func10952` | 19,286 | 1,065 | 22 |
| `func6337` | 17,439 | 1,823 | 17 |
| `func5310` | 14,028 | 0 | 37 |

Distribution: 220 functions over 1,000 lines hold 34% of all lines; 9 over 10,000 lines hold 12%.
The top three are flattened state machines (6 K arms each).

To test "one huge function is the cause", every function body except a chosen set was replaced
by `unimplemented!()` and the result went through `rustc` (stub script in the appendix):

| bodies kept | lines kept | time | peak RSS |
|---|---|---|---|
| none (all stubbed, data statics kept) | 155 K | 3.5 s | 1.3 GB |
| `func11932` only (largest) | 202 K | 7.9 s | 1.5 GB |
| top 3 (47 K + 46 K + 42 K lines) | 290 K | **20 s** | 2.1 GB |
| 220 functions > 1,000 lines | 796 K | 108 s | 3.8 GB |
| 1,138 functions > 300 lines | 1.25 M | 258 s | 5.4 GB |
| 3,875 functions > 100 lines | 1.70 M | 458 s | 7.2 GB |
| all 12,722 | 1.89 M | **587 s** | 8.3 GB |

The three biggest functions together cost 17 s over the baseline. Time grows with the total
amount of code (and super-linearly), not with any single body. **[verified]: not "a specific huge
function".** (Go for scale: 458 K lines, 64 s, 4.0 GB.)

## 2. The "4 hours" did not reproduce [verified]

Every configuration of the full JS source finished in about 9.4-9.8 minutes:

| flags | wall | user | sys | peak RSS |
|---|---|---|---|---|
| `-C opt-level=1` (default 16 CGUs) | 587 s | 281 s | 306 s | 8.3 GB |
| `-C opt-level=1 -C codegen-units=4` | 576 s | 278 s | 298 s | 8.3 GB |
| `-C opt-level=0` | 564 s | 269 s | 295 s | 7.6 GB |
| `-C opt-level=s` | 575 s | 277 s | 298 s | 8.3 GB |
| `-C opt-level=2` | 572 s | 290 s | 281 s | 11.3 GB |
| `-C opt-level=3` | 572 s | 290 s | 282 s | 11.4 GB |

The survey's "about 10 minutes" is right; #316's "over 4 hours, `cgu.0` stuck at 0 bytes" is
not reproduced with this `program.rs`, `rustc 1.99.1` and one process at a time. [inferred]
Likely causes of the earlier run: several 7-9 GB `rustc` processes at once (sys time already
equals user time here, so swapping would multiply it), or different flags. It is recorded as
"not reproduced", not as "fixed".

Opt level, CGU count, `--split-dispatch` and `--jobs` are irrelevant at this size (Go table in
section 5; the 4 MB-chunk rows of section 4B show O0-O3 spanning only 54-80 s), which already says the time is not in LLVM.

## 3. Where the time goes (step 3) [verified]

`-Z time-passes` (via `RUSTC_BOOTSTRAP=1`, JS `program.rs`, a 554 s run) labels only about 60 s of the 554 s (`parse_crate`
2.3 s / 2.5 GB, `type_check_crate` 9.8 s, `MIR_borrow_checking` 26 s, `generate_crate_metadata`
11.6 s, `LLVM_passes` 0.1 s). The rest is in an unlabeled phase. `sample` on the running `rustc`
at 150 s and at 350 s shows the main thread in the same place both times, with 5.4 K of 5.8 K
samples in `_platform_memmove`:

```
rustc_interface::passes::early_lint_checks -> rustc_lint::early::check_ast_node
  -> ... walk_stmt / visit_arms -> BuiltinCombinedEarlyLintPass::check_expr
  -> UnusedParens::emit_unused_delims_expr -> emit_unused_delims
  -> SourceMap::span_to_next_source -> _platform_memmove
```

`emit_unused_delims` asks `span_to_next_source` for the text after the closing parenthesis (to
decide whether to keep a space in the suggestion). That returns an owned `String` of **the rest
of the file**. With N redundant parentheses in a file of size S this is O(N*S).

Confirmed three ways:

1. **Generated code**: with `unused_parens` removed from the generated `#[allow(...)]`, the Go
   output gives 40,098 warnings (35,592 "around method argument", 4,428 "around assigned value",
   58 block tail, 16 function argument, 4 `return`).
2. **Micro-repro** (`#![allow(unused_parens)]`, 40 K functions of 10 lines
   `self.f((x.wrapping_add(j)), x);`, `--emit=metadata`, 17.5 MB):

   | statements | with `((..))` | without (same file, parentheses removed) |
   |---|---|---|
   | 100 K | 7.9 s | |
   | 200 K | 78 s (more than 4x: memory effects not isolated) | |
   | 400 K | **296 s** | **8.1 s** |

3. **Same input, only parentheses changed** (section 4): JS `program.rs` 587 s -> 61 s.

Lint levels do not help: `-A unused_parens` and `--cap-lints allow` gave 258.8 s and 259.0 s on
the 1,138-function stub (baseline 258.3 s), and the generated code already has
`#[allow(... unused_parens ...)]`. [inferred] The work happens before the level is checked.

Classification of the four candidate causes:

| candidate | verdict |
|---|---|
| LLVM optimization of huge functions | **No**: O0 = O3 = 570 s; top-3 functions add 17 s |
| state-machine `match` size | **No** (same evidence); only a cost inside the 20 s above |
| plain amount of code | **Partly**: it is the file size times the number of redundant parentheses, not the line count (Go 458 K lines: 64 s, 5.7x less after fix) |
| memory pressure | **A symptom, not the cause**: sys time (306 s) disappears with the fix (3 s) while RSS stays 8.2 GB |

The 8 GB peak is a separate floor: `parse_crate` alone is 2.5 GB for this 124 MB source, and
metadata generation adds more. It does not change with these fixes (8.2 GB after).

## 4. What fixes it [verified]

**A. Do not emit the redundant parentheses.** A throwaway post-processor (appendix) that strips
a fully wrapping `( ... )` (no top-level comma) in three positions - method/function argument,
assigned value (`x = (..);`, `let v: T = (..);`), `{ (..) }` block tail - was
run on the generated source (87 K lines changed in JS, 38 K in Go; the file shrinks by about 0.1-0.2%, a few hundred KB at most):

| input | before | after | after, `-C opt-level=0` |
|---|---|---|---|
| JS 124 MB single file, O1 | 587 s, 8.3 GB, sys 306 s | **61 s**, 8.2 GB, sys 3 s | 52 s, 7.6 GB |
| Go 29 MB, O1 | 64 s, 4.0 GB, sys 29 s | **11.4 s**, 4.0 GB, sys 0.4 s | |
| Go, `--emit=metadata` | 61 s | 9.6 s | |

Steps along the way on JS (metadata only): 181 s -> 68 s -> 48 s as the call-argument,
`let`/assignment and block-tail sites were removed; 192 `return (..)` and 158 other sites were
left and are cheap. So the cost is proportional to the number of remaining sites; it does not
need all of them gone.

**B. Several source files.** For the same JS core, `rustc` per layout:

| layout | files | wall | user | sys |
|---|---|---|---|---|
| single file (124 MB) | 1 | 568 s | 274 s | 293 s |
| `funcs_per_file=4000` | 6 (largest ~31 MB) | 209 s | 123 s | 86 s |
| `funcs_per_file=1000` | 15 (<= 18 MB) | 87 s | 72 s | 14 s |
| `funcs_per_file=200` | 66 | 64 s | 62 s | 1.6 s |
| `max_bytes_per_file=4 MB` | 28 | 63 s | 62 s | 1.5 s |
| 4 MB chunks, each `mod fN { include!("fN.rs"); }` | 28 | 62 s | 61 s | 1.4 s |

The last row matters: `include!` produces a separate source file per chunk, so
`transpile_component` / `include_component!` (which are single `include!`s today) can use it.
With 4 MB chunks the opt level matters again (O0 54 s, O1 62 s, O3 80 s): LLVM becomes about a
quarter of a now-small total.

A and B reach the same floor (about 60 s) because both remove the quadratic term. Combined was
not measured.

## 5. Existing levers (step 2) [verified]

On the Go component (`--component`, 29 MB, baseline O1 64 s):

| change | wall | verdict |
|---|---|---|
| `-C opt-level=0` / `s` | 63.9 / 65.6 s | no effect |
| `-C codegen-units=1` / `64` | 65.0 / 65.0 s | no effect |
| `--split-dispatch=500` / `100` | 65.4 / 67.4 s | no effect (size grows 29.4 / 30.2 MB) |
| `--jobs=1` / `0` (transpile) | 0.19 / 0.06 s; output byte-identical | irrelevant: transpiling is not the bottleneck |

`--split-dispatch` splits a flat `match` into part functions; it targets LLVM time on huge
dispatch functions, which is not the problem here. The `--component` JS transpile does not run
at all today (`unsupported: the component's imports are lowered with different realloc`, issue
#314); the JS numbers above come from the earlier `program.rs` and the equivalent core module.

## 6. Options (step 4)

| option | effect | effort | risk |
|---|---|---|---|
| **A. No redundant parentheses** at argument / assignment / block-tail sites | JS 9.3x, Go 5.7x; no layout or API change; source about 0.1-0.2% smaller | **small to medium**: one `strip_outer_parens` helper (balanced, no top-level comma, not a tuple) applied where `Val.code` is spliced into a statement; about 22 `join(", ")` sites in `func/`, `render.rs` `let`/assign emitters, block-tail rendering. Golden-test churn: all expected outputs lose parentheses | low: removal only at positions where the lint proves the parentheses are redundant; `cargo test` and the 500-test e2e suite catch a wrong strip. Must keep parentheses for tuples and receivers (`(-2i32).rotate_left`) |
| B. Split output for `--component` | same ~60 s; same ~60 s; peak RSS was not recorded per layout (the 62-64 s runs peaked at 8.1-8.6 GB) | medium: the glue and `Instance` live in one source; chunks via `include!`; `--component` refuses split args today and `include_component!` assumes one file | medium (module paths, `use super::*`) |
| C. `opt-level=0` path / auto CGU | no help (section 2) | - | rejected |
| D. `--split-dispatch` / outline huge functions | no help (section 1) | - | rejected |
| E. de-flatten / pc-edge work (P1/P2) | no help for this problem; `googlesql`'s 31 min was a different (LLVM) cost | large | rejected for F4 |
| F. reduce generated size | the lint is per site, not per byte; A already does the useful part | medium | covered by A |
| G. upstream `rustc` | `span_to_next_source` should not copy to end of file | out of scope (report) | - |

**Recommendation: A**, as F4-1, then re-measure the real JS component (blocked on #314 for an
end-to-end run) and decide on B only if memory (8 GB) or build parallelism still matters.
A is also the better base for F2-5 (Python, 116 MB): it needs no change to `--component`
output structure.

## Not measured / scope

- **Python (`py-hello.wasm`)**: its program module imports memory (the F2 link step is what
  merges it into the 116 MB source), so it cannot be transpiled alone; no run was possible here.
  [inferred] It has the same generated-code style, so the same quadratic term applies; confirm
  after F2-4.
- The 4-hour run and its `cgu.0` (section 2); `-Z time-passes` leaves the lint phase unlabeled.
- Peak RSS ~8 GB (parse + metadata) is unchanged by A or B.
- Out of scope, to file separately: a `rustc` issue for `span_to_next_source` (micro-repro in
  section 3); #314 (realloc) blocks `--component` on the JS component.

## Appendix: scripts used (not committed)

Stub every function except a chosen set (section 1):

```python
import re, sys
src, dst, keep = sys.argv[1], sys.argv[2], set(sys.argv[3:])
pat = re.compile(r'^    (?:pub )?fn (func\d+)\(.*\{$')
out, skip = [], False
for l in open(src, errors='surrogateescape').read().split('\n'):
    if skip:
        if l == '    }': out.append(l); skip = False
        continue
    m = pat.match(l)
    if m and m.group(1) not in keep:
        out += [l, '        unimplemented!()']; skip = True; continue
    out.append(l)
open(dst, 'w', errors='surrogateescape').write('\n'.join(out))
```

Parenthesis stripper (section 4A; line-based, skips lines with string literals):

```python
import sys, re
def match_close(s, i):
    d = 0
    for j in range(i, len(s)):
        d += (s[j] == '(') - (s[j] == ')')
        if d == 0: return j
    return -1
def top_comma(t):
    d = 0
    for c in t:
        if c in '([{': d += 1
        elif c in ')]}': d -= 1
        elif c == ',' and d == 0: return True
    return False
def strip(s):
    out, i, n = [], 0, len(s)
    while i < n:
        c = s[i]
        if c == '(' and i > 0 and (s[i-1] == '(' or s[i-2:i] in (', ', '{ ')):
            j = match_close(s, i)
            tail_ok = j > 0 and (s[j+1:j+2] in (',', ')') if s[i-2:i] != '{ ' else s[j+1:j+3] == ' }')
            if tail_ok and s[i+1:j] and not top_comma(s[i+1:j]):
                out.append(strip(s[i+1:j])); i = j + 1; continue
        out.append(c); i += 1
    return ''.join(out)
asg = re.compile(r'^(\s*)((?:let (?:mut )?)?[A-Za-z_][\w.]*(?:: [A-Za-z0-9_<>;\[\] ]+)? = )\((.*)\);$')
for l in sys.stdin:
    l = l.rstrip('\n')
    if not ('b"' in l or '"' in l or l.startswith(('static', 'const')) or "'('" in l or "')'" in l):
        m = asg.match(l)
        if m and match_close('(' + m.group(3) + ')', 0) == len(m.group(3)) + 1 and not top_comma(m.group(3)):
            l = m.group(1) + m.group(2) + m.group(3) + ';'
        l = strip(l)
    print(l)
```

Reproduction: `wasm2rs --extract-core c.wasm d/`; `wasm2rs --named-imports --named-exports
--reentrant-imports d/module0.wasm out.rs [funcs_per_file [max_bytes_per_file]]`;
`rustc --edition 2021 --crate-type lib -C opt-level=1 out.rs` (or `split/lib.rs`); `sample
<rustc pid> 8` while it runs to see the lint frames.
