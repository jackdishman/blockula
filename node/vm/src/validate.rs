//! Upload validation (docs/PROGRAMS.md 4.2): the rules a program's code must pass before any
//! engine sees it, Blockula's own and independent of the engine. `UploadCode` runs [`validate`]
//! first and a test translation in a throwaway engine second (4.3, 7.1); calls later re-parse
//! stored bytes that already passed both.
//!
//! wasmparser's validator runs under [`FEATURES`] (Wasm 1.0, which is MVP plus mutable globals,
//! without floats: what `wasm32v1-none` emits). It doesn't catch everything that profile excludes,
//! so this module also walks every section and every operator itself, before the validator sees
//! them:
//! - **Floats anywhere.** With floats off, wasmparser still accepts `i32.trunc_f32_s`,
//!   `i64.reinterpret_f64` and the other float-to-int conversions in unreachable code, where the
//!   operand stack is polymorphic. Operators are checked against an allowlist of Wasm 1.0's
//!   integer subset, reachable or not, and value types everywhere (signatures, locals, globals,
//!   blocks).
//! - **Encodings and sections from later proposals** that wasmparser accepts with those
//!   proposals off: the data count section, passive data segments (`add_data_segment` accepts
//!   them whatever the features), and the bulk-memory encodings of data and element segments.
//! - **Blockula's own rules**: imports, the one fixed memory and no `memory.grow` (reachable or
//!   not), tables, limits, data, exports, no start function, no custom sections.
//!
//! The limits are at least as strict as wasmi's `EnforcedLimits::strict()` (soroban-wasmi has no
//! such switch), and a module that passes has nothing the engine's `Module::new` refuses under the
//! 4.2 settings (the tests check it). It never panics: counts are checked before they're used,
//! sums are checked, nothing indexes a slice without a bound, and every error from wasmparser
//! becomes a [`ValidateError`].

use crate::{abi, config};
use std::fmt;
use wasmparser::{
    BinaryReader, BinaryReaderError, BlockType, CompositeInnerType, ConstExpr, DataKind,
    ElementItems, ElementKind, Encoding, ExternalKind, FuncType, FuncValidator,
    FuncValidatorAllocations, FunctionBody, Operator, Parser, Payload, RefType, TableInit, TypeRef,
    ValType, ValidPayload, Validator, ValidatorResources, WasmFeatures,
};

/// The Wasm feature set: Wasm 1.0 (MVP plus mutable globals) without floats. `GC_TYPES` stays on
/// because wasmparser needs it for MVP's `funcref` tables.
pub const FEATURES: WasmFeatures = WasmFeatures::WASM1.difference(WasmFeatures::FLOATS);

/// Parameters of any function type: wasmi's `EnforcedLimits::strict()` value, enforced here since
/// soroban-wasmi enforces none (the largest host function takes 8).
pub const MAX_PARAMS: usize = 32;
/// Data and element segments, each: `EnforcedLimits::strict()`'s limits, for the same reason.
pub const MAX_DATA_SEGMENTS: u32 = 1_000;
pub const MAX_ELEMENT_SEGMENTS: u32 = 1_000;
/// Operand stack height anywhere in a function, as wasmparser's validator counts it (unreachable
/// code included). Compiled code stays in the tens; the bound keeps every function's frame small
/// for any engine a later protocol version might pin (register-based wasmi frames give a function
/// at most 65,535 slots, two per local and one per operand) without refusing any real program.
pub const MAX_OPERAND_STACK: u32 = 4_096;

/// A Wasm page, in bytes.
const PAGE_BYTES: u64 = 1 << 16;
/// Longest name or message quoted in an error: names come from the upload, and errors end up in
/// receipts and explorers.
const MAX_QUOTED: usize = 64;

/// What [`validate`] learned about a module that passed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleInfo {
    /// Method names: the `bk_` function exports without the prefix, sorted, each an
    /// [`abi::is_ident`]. `UploadCode` matches them one to one with the interface (7.1).
    pub exports: Vec<String>,
    /// Memory pages, initial = maximum (at most [`config::MAX_PAGES`]).
    pub pages: u32,
    /// Bytes of data segments, which every instantiation copies (`meter::Schedule::instantiate_fuel`).
    pub data_bytes: usize,
    /// The host functions imported, in import order: `imports[i]` is function index `i`. No
    /// function is imported twice.
    pub imports: Vec<&'static str>,
}

/// Which rule a module broke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// Larger than [`config::MAX_CODE_BYTES`].
    Size,
    /// Malformed, or refused by wasmparser's validator under [`FEATURES`] (types, stack,
    /// indices, section order…).
    Wasm,
    /// An instruction, type, section or encoding outside Wasm 1.0's integer subset, other than a
    /// float (sign extension, bulk memory, reference types, multi-value, SIMD…).
    Feature,
    /// A float type or instruction, reachable or not.
    Float,
    /// An import that isn't an ABI v1 host function with its exact signature, or a duplicate.
    Import,
    /// Not exactly one memory, defined, exported as `memory`, initial = maximum ≤ `MAX_PAGES`,
    /// 32-bit, unshared, 64 KiB pages.
    Memory,
    /// A `memory.grow`, reachable or not. Refused rather than left to fail at run time:
    /// - the memory is fixed (initial = maximum), so a grow could only ever return −1 or grow by
    ///   0 pages: it does nothing a program needs (`memory.size` stays allowed);
    /// - what a refused grow costs is the engine's own (soroban-wasmi charges 1,024 fuel per
    ///   requested page before refusing; wasmi 0.38 refuses at its static cost), so refusing it
    ///   here keeps the outcome independent of the engine and its version.
    MemoryGrow,
    /// More than one table, more than `MAX_TABLE_ENTRIES`, or not a plain `funcref` table.
    Table,
    /// Functions, locals, globals, parameters, segments or operand stack height past their limit.
    Limit,
    /// Data segments larger in total than the code section, or a data or element segment that
    /// doesn't fit its memory or table (the module could never be instantiated).
    Segment,
    /// The average function body is smaller than [`config::MIN_AVG_BODY_BYTES`] in a code
    /// section of at least [`config::MIN_AVG_BODY_FROM`] bytes.
    BodySize,
    /// An export other than `memory` and functions `bk_<ident>` of type `[] -> []`.
    Export,
    /// A start section.
    Start,
    /// A custom section. Refused rather than ignored:
    /// - soroban-wasmi has no `ignore_custom_sections` and copies every one into each `Module`
    ///   (`Module::custom_sections()`), so ignoring them would hold on one engine only;
    /// - they do nothing at run time, yet they'd be stored, hashed and loaded on every block that
    ///   declares the program, and two uploads differing only in one would get different code
    ///   hashes for the same behavior, which defeats deduplication and verifiable builds (7.5);
    /// - they carry arbitrary bytes (and text an explorer might show) at code prices;
    /// - `blockula program build` strips them (4.3 names offending functions from the name
    ///   section before stripping it), as `strip = true` does for rustc's `name`, `producers`
    ///   and `target_features`.
    ///
    /// Bounding their size instead would keep the hash ambiguity for no benefit.
    CustomSection,
}

/// Why [`validate`] refused a module: the rule, where, and a message for the uploader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidateError {
    pub rule: Rule,
    /// The function the problem is in, by its index in the function index space (imports
    /// first, as `wasm-tools print` numbers them), when there is one.
    pub func: Option<u32>,
    /// Byte offset into the uploaded code, when there is one.
    pub offset: Option<usize>,
    /// What's wrong, with any name from the module quoted, escaped and truncated.
    pub message: String,
}

impl fmt::Display for ValidateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        match (self.func, self.offset) {
            (Some(func), Some(offset)) => write!(f, " (function {func}, at offset {offset:#x})"),
            (Some(func), None) => write!(f, " (function {func})"),
            (None, Some(offset)) => write!(f, " (at offset {offset:#x})"),
            (None, None) => Ok(()),
        }
    }
}

impl std::error::Error for ValidateError {}

fn err(rule: Rule, offset: usize, message: impl Into<String>) -> ValidateError {
    ValidateError { rule, func: None, offset: Some(offset), message: message.into() }
}

impl From<BinaryReaderError> for ValidateError {
    fn from(e: BinaryReaderError) -> Self {
        // wasmparser's messages can quote names from the module (duplicate exports, for one).
        err(Rule::Wasm, e.offset(), bounded(e.message()))
    }
}

/// `s` escaped and cut to [`MAX_QUOTED`] characters, for quoting untrusted text in a message.
fn quoted(s: &str) -> String {
    let cut: String = s.chars().take(MAX_QUOTED).collect();
    if cut.len() < s.len() { format!("{cut:?}…") } else { format!("{cut:?}") }
}

/// A message from wasmparser, with control characters replaced and cut to a bounded length.
fn bounded(s: &str) -> String {
    let max = 4 * MAX_QUOTED;
    let mut out: String = s.chars().take(max).map(|c| if c.is_control() { '?' } else { c }).collect();
    if s.chars().nth(max).is_some() {
        out.push('…');
    }
    out
}

/// Validates an upload against PROGRAMS.md 4.2 and returns what an engine and the fee schedule
/// need to know about it. Never panics.
pub fn validate(wasm: &[u8]) -> Result<ModuleInfo, ValidateError> {
    if wasm.len() > config::MAX_CODE_BYTES {
        return Err(ValidateError {
            rule: Rule::Size,
            func: None,
            offset: None,
            message: format!("code is {} bytes, more than {}", wasm.len(), config::MAX_CODE_BYTES),
        });
    }
    let mut parser = Parser::new(0);
    // Decode as Wasm 1.0 too, not just validate: with every feature on (`Parser::new`'s default),
    // `call_indirect`'s table byte and `memory.size`/`memory.grow`'s memory byte decode as LEBs,
    // where Wasm 1.0 requires a single zero byte.
    parser.set_features(FEATURES);
    let mut validator = Validator::new_with_features(FEATURES);
    let mut checker = Checker { wasm, ..Checker::default() };
    let mut allocs = FuncValidatorAllocations::default();
    for payload in parser.parse_all(wasm) {
        let payload = payload?;
        // Our rules first, so a float is always `Rule::Float` and every count is bounded before
        // wasmparser reserves storage for it.
        checker.section(&payload)?;
        match validator.payload(&payload)? {
            ValidPayload::Ok => {}
            ValidPayload::Func(func, body) => {
                let mut fv = func.into_validator(std::mem::take(&mut allocs));
                checker.function(&mut fv, &body).map_err(|mut e| {
                    e.func = Some(fv.index());
                    e
                })?;
                allocs = fv.into_allocations();
            }
            ValidPayload::End(_) => return checker.end(),
            // Nested modules and components: the component model isn't compiled in.
            ValidPayload::Parser(_) => return Err(err(Rule::Feature, 0, "nested modules are outside Wasm 1.0")),
        }
    }
    // `parse_all` ends with `End` or an error; a module that stops short is malformed.
    Err(err(Rule::Wasm, wasm.len(), "unexpected end of module"))
}

/// Section-by-section state: what later sections are checked against.
#[derive(Default)]
struct Checker<'a> {
    wasm: &'a [u8],
    /// The type section's function types, by type index.
    types: Vec<FuncType>,
    /// Each defined function's type index, in order.
    funcs: Vec<u32>,
    /// Host functions imported, in order (they're the first function indices).
    imports: Vec<&'static str>,
    memories: u32,
    pages: u32,
    memory_exported: bool,
    /// The table's entries, if there is one (it never grows: `table.grow` is reference types).
    table: Option<u64>,
    /// Bytes of function bodies (the code section after its count, as wasmi measures it).
    code_bytes: u64,
    data_bytes: u64,
    exports: Vec<String>,
}

impl Checker<'_> {
    /// Checks one section, before wasmparser's validator sees it.
    fn section(&mut self, payload: &Payload<'_>) -> Result<(), ValidateError> {
        match payload {
            Payload::Version { encoding, range, .. } => {
                if *encoding != Encoding::Module {
                    return Err(err(Rule::Feature, range.start, "a component, not a core module"));
                }
            }
            Payload::TypeSection(s) => {
                declared(s.count(), s.range())?;
                for group in s.clone().into_iter_with_offsets() {
                    let (offset, group) = group?;
                    for sub in group.into_types() {
                        let CompositeInnerType::Func(ty) = sub.composite_type.inner else {
                            return Err(err(Rule::Feature, offset, "only function types are in Wasm 1.0"));
                        };
                        for t in ty.params().iter().chain(ty.results()) {
                            value_type(*t, offset)?;
                        }
                        if ty.results().len() > 1 {
                            return Err(err(Rule::Feature, offset, "more than one result (multi-value)"));
                        }
                        if ty.params().len() > MAX_PARAMS {
                            return Err(err(Rule::Limit, offset, format!("{} parameters, more than {MAX_PARAMS}", ty.params().len())));
                        }
                        self.types.push(ty);
                    }
                }
            }
            Payload::ImportSection(s) => {
                declared(s.count(), s.range())?;
                for import in s.clone().into_iter_with_offsets() {
                    let (offset, import) = import?;
                    let what = || format!("{}.{}", quoted(import.module), quoted(import.name));
                    let TypeRef::Func(ty) = import.ty else {
                        return Err(err(Rule::Import, offset, format!("import {} isn't a function: programs import only host functions", what())));
                    };
                    let host = abi::host_fn(import.name).filter(|_| import.module == abi::MODULE).ok_or_else(|| {
                        err(Rule::Import, offset, format!("import {} isn't a host function of ABI v1", what()))
                    })?;
                    let signature_matches = self.types.get(ty as usize).is_some_and(|ty| {
                        same_types(ty.params(), host.params) && same_types(ty.results(), host.results)
                    });
                    if !signature_matches {
                        return Err(err(Rule::Import, offset, format!("import {} doesn't have ABI v1's signature {:?} -> {:?}", what(), host.params, host.results)));
                    }
                    if self.imports.contains(&host.name) {
                        return Err(err(Rule::Import, offset, format!("{} is imported twice", what())));
                    }
                    self.imports.push(host.name);
                }
            }
            Payload::FunctionSection(s) => {
                declared(s.count(), s.range())?;
                if s.count() > config::MAX_FUNCS {
                    return Err(err(Rule::Limit, s.range().start, format!("{} functions, more than {}", s.count(), config::MAX_FUNCS)));
                }
                for ty in s.clone() {
                    self.funcs.push(ty?);
                }
            }
            Payload::TableSection(s) => {
                declared(s.count(), s.range())?;
                if s.count() > 1 {
                    return Err(err(Rule::Table, s.range().start, format!("{} tables, more than one", s.count())));
                }
                for table in s.clone().into_iter_with_offsets() {
                    let (offset, table) = table?;
                    let ty = table.ty;
                    if ty.element_type != RefType::FUNCREF || ty.table64 || ty.shared || !matches!(table.init, TableInit::RefNull) {
                        return Err(err(Rule::Table, offset, "the table must be a plain 32-bit funcref table"));
                    }
                    let max = u64::from(config::MAX_TABLE_ENTRIES);
                    if ty.initial > max || ty.maximum.is_some_and(|m| m > max) {
                        return Err(err(Rule::Table, offset, format!("a table of more than {max} entries")));
                    }
                    self.table = Some(ty.initial);
                }
            }
            Payload::MemorySection(s) => {
                declared(s.count(), s.range())?;
                if s.count() > 1 {
                    return Err(err(Rule::Memory, s.range().start, format!("{} memories, more than one", s.count())));
                }
                for memory in s.clone().into_iter_with_offsets() {
                    let (offset, ty) = memory?;
                    if ty.memory64 || ty.shared || ty.page_size_log2.is_some() {
                        return Err(err(Rule::Memory, offset, "the memory must be 32-bit, unshared, with 64 KiB pages"));
                    }
                    if ty.maximum != Some(ty.initial) || ty.initial > u64::from(config::MAX_PAGES) {
                        return Err(err(Rule::Memory, offset, format!("the memory must have initial = maximum ≤ {} pages: it never grows", config::MAX_PAGES)));
                    }
                    self.memories += 1;
                    self.pages = ty.initial as u32; // ≤ MAX_PAGES, checked above
                }
            }
            Payload::TagSection(s) => {
                return Err(err(Rule::Feature, s.range().start, "a tag section (exceptions)"));
            }
            Payload::GlobalSection(s) => {
                declared(s.count(), s.range())?;
                if s.count() > config::MAX_GLOBALS {
                    return Err(err(Rule::Limit, s.range().start, format!("{} globals, more than {}", s.count(), config::MAX_GLOBALS)));
                }
                for global in s.clone().into_iter_with_offsets() {
                    let (offset, global) = global?;
                    value_type(global.ty.content_type, offset)?;
                    if global.ty.shared {
                        return Err(err(Rule::Feature, offset, "a shared global"));
                    }
                    constant(&global.init_expr)?;
                }
            }
            Payload::ExportSection(s) => {
                declared(s.count(), s.range())?;
                for export in s.clone().into_iter_with_offsets() {
                    let (offset, export) = export?;
                    self.export(offset, export.name, export.kind, export.index)?;
                }
            }
            Payload::StartSection { range, .. } => {
                return Err(err(Rule::Start, range.start, "a start function: programs run only when called"));
            }
            Payload::ElementSection(s) => {
                declared(s.count(), s.range())?;
                if s.count() > MAX_ELEMENT_SEGMENTS {
                    return Err(err(Rule::Limit, s.range().start, format!("{} element segments, more than {MAX_ELEMENT_SEGMENTS}", s.count())));
                }
                for element in s.clone() {
                    let element = element?;
                    let offset = element.range.start;
                    // Wasm 1.0's only form: active, table 0 implied, function indices (flags 0).
                    let (ElementKind::Active { table_index: None, offset_expr }, ElementItems::Functions(items)) = (&element.kind, &element.items) else {
                        return Err(err(Rule::Feature, offset, "an element segment outside Wasm 1.0's form (bulk memory, reference types)"));
                    };
                    let start = u64::from(constant(offset_expr)?.ok_or_else(|| err(Rule::Wasm, offset, "element offset isn't an i32"))?);
                    let entries = self.table.unwrap_or(0);
                    if start + u64::from(items.count()) > entries {
                        return Err(err(Rule::Segment, offset, format!("element segment at {start} of {} entries doesn't fit the table of {entries}", items.count())));
                    }
                }
            }
            Payload::DataCountSection { range, .. } => {
                return Err(err(Rule::Feature, range.start, "a data count section (bulk memory)"));
            }
            Payload::CodeSectionStart { count, range, size } => {
                declared(*count, range.clone())?;
                if *count > config::MAX_FUNCS {
                    return Err(err(Rule::Limit, range.start, format!("{count} function bodies, more than {}", config::MAX_FUNCS)));
                }
                // wasmi's measure: the section's bytes after its count, over the bodies (checked
                // division: nothing here can divide by zero).
                if *size >= config::MIN_AVG_BODY_FROM {
                    match size.checked_div(*count) {
                        Some(avg) if avg >= config::MIN_AVG_BODY_BYTES => {}
                        avg => {
                            return Err(err(Rule::BodySize, range.start, format!("{count} function bodies in {size} bytes average {} bytes, under {}", avg.unwrap_or(0), config::MIN_AVG_BODY_BYTES)));
                        }
                    }
                }
                self.code_bytes = u64::from(*size);
            }
            Payload::DataSection(s) => {
                declared(s.count(), s.range())?;
                if s.count() > MAX_DATA_SEGMENTS {
                    return Err(err(Rule::Limit, s.range().start, format!("{} data segments, more than {MAX_DATA_SEGMENTS}", s.count())));
                }
                for data in s.clone() {
                    let data = data?;
                    let offset = data.range.start;
                    // Wasm 1.0's form starts with memory index 0 (bulk memory's flags 0); flags 2
                    // name the memory explicitly, flags 1 are passive. wasmparser accepts all three.
                    let flags = self.wasm.get(data.range.clone()).and_then(|b| BinaryReader::new(b, offset).read_var_u32().ok());
                    let DataKind::Active { memory_index: 0, offset_expr } = &data.kind else {
                        return Err(err(Rule::Feature, offset, "a passive data segment, or one for another memory (bulk memory)"));
                    };
                    if flags != Some(0) {
                        return Err(err(Rule::Feature, offset, "a data segment outside Wasm 1.0's form (bulk memory)"));
                    }
                    let start = u64::from(constant(offset_expr)?.ok_or_else(|| err(Rule::Wasm, offset, "data offset isn't an i32"))?);
                    let len = data.data.len() as u64;
                    let memory = u64::from(self.pages) * PAGE_BYTES;
                    if start + len > memory {
                        return Err(err(Rule::Segment, offset, format!("data segment at {start} of {len} bytes doesn't fit the memory of {memory}")));
                    }
                    self.data_bytes += len;
                    if self.data_bytes > self.code_bytes {
                        return Err(err(Rule::Segment, offset, format!("data segments total {} bytes, more than the code's {}", self.data_bytes, self.code_bytes)));
                    }
                }
            }
            Payload::CustomSection(s) => {
                return Err(err(Rule::CustomSection, s.range().start, format!("a custom section {}: strip custom sections before uploading (`blockula program build` does)", quoted(s.name()))));
            }
            Payload::CodeSectionEntry(_) | Payload::UnknownSection { .. } | Payload::End(_) => {}
            // `Payload` is non-exhaustive; component payloads aren't compiled in.
            _ => return Err(err(Rule::Feature, 0, "a section outside Wasm 1.0")),
        }
        Ok(())
    }

    /// Checks one export.
    fn export(&mut self, offset: usize, name: &str, kind: ExternalKind, index: u32) -> Result<(), ValidateError> {
        if name == "memory" {
            if kind != ExternalKind::Memory {
                return Err(err(Rule::Export, offset, "the export \"memory\" must be the memory"));
            }
            self.memory_exported = true;
            return Ok(());
        }
        let Some(method) = name.strip_prefix(abi::EXPORT_PREFIX).filter(|m| abi::is_ident(m)) else {
            return Err(err(Rule::Export, offset, format!("export {} is neither \"memory\" nor bk_ and a method name [a-z][a-z0-9_]{{0,31}}", quoted(name))));
        };
        if kind != ExternalKind::Func {
            return Err(err(Rule::Export, offset, format!("export {} isn't a function", quoted(name))));
        }
        // A method must be the program's own code: re-exporting a host function would let a call
        // enter the host with no program frame.
        let defined = index.checked_sub(self.imports.len() as u32).ok_or_else(|| {
            err(Rule::Export, offset, format!("export {} is an imported function", quoted(name)))
        })?;
        let ty = self.funcs.get(defined as usize).and_then(|ty| self.types.get(*ty as usize));
        if !ty.is_some_and(|ty| ty.params().is_empty() && ty.results().is_empty()) {
            return Err(err(Rule::Export, offset, format!("method {} must have type [] -> []: input and output go through the host", quoted(name))));
        }
        self.exports.push(method.to_string());
        Ok(())
    }

    /// Checks one function body (locals, then every operator, reachable or not) in step with
    /// wasmparser's function validator.
    fn function(&self, fv: &mut FuncValidator<ValidatorResources>, body: &FunctionBody<'_>) -> Result<(), ValidateError> {
        let defined = fv.index().checked_sub(self.imports.len() as u32);
        let params = defined
            .and_then(|i| self.funcs.get(i as usize))
            .and_then(|ty| self.types.get(*ty as usize))
            .map_or(0, |ty| ty.params().len());
        let mut reader = body.get_binary_reader();
        reader.set_features(FEATURES);
        // Params count as locals. Counts are u32s that can sum past u32::MAX, so add in u64 and
        // stop at the first group over the limit.
        let mut locals = params as u64;
        for _ in 0..reader.read_var_u32()? {
            let offset = reader.original_position();
            let count = reader.read_var_u32()?;
            let ty: ValType = reader.read()?;
            locals += u64::from(count);
            if locals > u64::from(config::MAX_LOCALS) {
                return Err(err(Rule::Limit, offset, format!("more than {} locals (parameters included)", config::MAX_LOCALS)));
            }
            value_type(ty, offset)?;
            fv.define_locals(offset, count, ty)?;
        }
        while !reader.eof() {
            let offset = reader.original_position();
            let op = reader.read_operator()?;
            operator(&op, offset)?;
            fv.op(offset, &op)?;
            if fv.operand_stack_height() > MAX_OPERAND_STACK {
                return Err(err(Rule::Limit, offset, format!("an operand stack deeper than {MAX_OPERAND_STACK}")));
            }
        }
        fv.finish(reader.original_position())?;
        Ok(())
    }

    /// The module-wide checks, once every section is in.
    fn end(mut self) -> Result<ModuleInfo, ValidateError> {
        let end = self.wasm.len();
        if self.memories != 1 {
            return Err(err(Rule::Memory, end, "no memory: a program defines exactly one"));
        }
        if !self.memory_exported {
            return Err(err(Rule::Memory, end, "the memory isn't exported as \"memory\""));
        }
        self.exports.sort();
        Ok(ModuleInfo {
            exports: self.exports,
            pages: self.pages,
            data_bytes: self.data_bytes as usize, // ≤ MAX_CODE_BYTES
            imports: self.imports,
        })
    }
}

/// Refuses a section declaring more entries than it has bytes (every entry takes at least one),
/// before anything reserves storage by the declared count. soroban-wasmi 29 checks the same
/// (`check_declared_count`).
fn declared(count: u32, range: std::ops::Range<usize>) -> Result<(), ValidateError> {
    if count as usize > range.len() {
        return Err(err(Rule::Wasm, range.start, format!("a section declares {count} entries in {} bytes", range.len())));
    }
    Ok(())
}

fn same_types(wasm: &[ValType], abi: &[abi::Ty]) -> bool {
    wasm.len() == abi.len()
        && wasm.iter().zip(abi).all(|(w, a)| matches!((w, a), (ValType::I32, abi::Ty::I32) | (ValType::I64, abi::Ty::I64)))
}

/// Value types are `i32` and `i64`, everywhere.
fn value_type(ty: ValType, offset: usize) -> Result<(), ValidateError> {
    match ty {
        ValType::I32 | ValType::I64 => Ok(()),
        ValType::F32 | ValType::F64 => Err(err(Rule::Float, offset, format!("a float type {ty}: floats are off"))),
        _ => Err(err(Rule::Feature, offset, format!("a value type {ty} outside Wasm 1.0's integers"))),
    }
}

/// A constant expression must be one `i32.const` or `i64.const` (Wasm 1.0 also allows
/// `global.get` of an imported global, and programs import none). Returns an `i32`'s value as the
/// unsigned address Wasm reads it as, or `None` for an `i64`.
fn constant(expr: &ConstExpr<'_>) -> Result<Option<u32>, ValidateError> {
    let not_one_const = |offset| err(Rule::Feature, offset, "a constant expression other than one i32.const or i64.const");
    let mut ops = expr.get_operators_reader();
    let (op, offset) = ops.read_with_offset()?;
    let value = match op {
        Operator::I32Const { value } => Some(value as u32),
        Operator::I64Const { .. } => None,
        Operator::F32Const { .. } | Operator::F64Const { .. } => {
            return Err(err(Rule::Float, offset, "a float constant: floats are off"));
        }
        _ => return Err(not_one_const(offset)),
    };
    let (end, offset) = ops.read_with_offset()?;
    if !matches!(end, Operator::End) {
        return Err(not_one_const(offset));
    }
    Ok(value)
}

/// Every operator must be in Wasm 1.0's integer subset, and not `memory.grow`, reachable or not.
/// An allowlist, so nothing a later wasmparser decodes can slip through.
fn operator(op: &Operator<'_>, offset: usize) -> Result<(), ValidateError> {
    use Operator::*;
    match op {
        Unreachable | Nop | Else | End | Br { .. } | BrIf { .. } | BrTable { .. } | Return
        | Call { .. } | CallIndirect { .. } | Drop | Select
        | LocalGet { .. } | LocalSet { .. } | LocalTee { .. } | GlobalGet { .. } | GlobalSet { .. }
        | I32Load { .. } | I64Load { .. }
        | I32Load8S { .. } | I32Load8U { .. } | I32Load16S { .. } | I32Load16U { .. }
        | I64Load8S { .. } | I64Load8U { .. } | I64Load16S { .. } | I64Load16U { .. } | I64Load32S { .. } | I64Load32U { .. }
        | I32Store { .. } | I64Store { .. } | I32Store8 { .. } | I32Store16 { .. }
        | I64Store8 { .. } | I64Store16 { .. } | I64Store32 { .. }
        | MemorySize { .. } | I32Const { .. } | I64Const { .. }
        | I32Eqz | I32Eq | I32Ne | I32LtS | I32LtU | I32GtS | I32GtU | I32LeS | I32LeU | I32GeS | I32GeU
        | I64Eqz | I64Eq | I64Ne | I64LtS | I64LtU | I64GtS | I64GtU | I64LeS | I64LeU | I64GeS | I64GeU
        | I32Clz | I32Ctz | I32Popcnt | I32Add | I32Sub | I32Mul | I32DivS | I32DivU | I32RemS | I32RemU
        | I32And | I32Or | I32Xor | I32Shl | I32ShrS | I32ShrU | I32Rotl | I32Rotr
        | I64Clz | I64Ctz | I64Popcnt | I64Add | I64Sub | I64Mul | I64DivS | I64DivU | I64RemS | I64RemU
        | I64And | I64Or | I64Xor | I64Shl | I64ShrS | I64ShrU | I64Rotl | I64Rotr
        | I32WrapI64 | I64ExtendI32S | I64ExtendI32U => Ok(()),
        Block { blockty } | Loop { blockty } | If { blockty } => match blockty {
            BlockType::Empty => Ok(()),
            BlockType::Type(ty) => value_type(*ty, offset),
            BlockType::FuncType(_) => Err(err(Rule::Feature, offset, "a block with a function type (multi-value)")),
        },
        // Wasm 1.0, but never in a program: see `Rule::MemoryGrow`.
        MemoryGrow { .. } => Err(err(Rule::MemoryGrow, offset, "a memory.grow: the memory is fixed (initial = maximum) and never grows")),
        _ if is_float(op) => Err(err(Rule::Float, offset, format!("a float instruction {}: floats are off", name(op)))),
        _ => Err(err(Rule::Feature, offset, format!("an instruction {} outside Wasm 1.0", name(op)))),
    }
}

/// Whether `op` takes or makes a float: the float instructions, conversions and reinterprets of
/// Wasm 1.0, the saturating truncations, and a typed `select` of a float.
fn is_float(op: &Operator<'_>) -> bool {
    use Operator::*;
    matches!(
        op,
        F32Load { .. } | F64Load { .. } | F32Store { .. } | F64Store { .. } | F32Const { .. } | F64Const { .. }
            | F32Eq | F32Ne | F32Lt | F32Gt | F32Le | F32Ge | F64Eq | F64Ne | F64Lt | F64Gt | F64Le | F64Ge
            | F32Abs | F32Neg | F32Ceil | F32Floor | F32Trunc | F32Nearest | F32Sqrt
            | F32Add | F32Sub | F32Mul | F32Div | F32Min | F32Max | F32Copysign
            | F64Abs | F64Neg | F64Ceil | F64Floor | F64Trunc | F64Nearest | F64Sqrt
            | F64Add | F64Sub | F64Mul | F64Div | F64Min | F64Max | F64Copysign
            | I32TruncF32S | I32TruncF32U | I32TruncF64S | I32TruncF64U
            | I64TruncF32S | I64TruncF32U | I64TruncF64S | I64TruncF64U
            | F32ConvertI32S | F32ConvertI32U | F32ConvertI64S | F32ConvertI64U | F32DemoteF64
            | F64ConvertI32S | F64ConvertI32U | F64ConvertI64S | F64ConvertI64U | F64PromoteF32
            | I32ReinterpretF32 | I64ReinterpretF64 | F32ReinterpretI32 | F64ReinterpretI64
            | I32TruncSatF32S | I32TruncSatF32U | I32TruncSatF64S | I32TruncSatF64U
            | I64TruncSatF32S | I64TruncSatF32U | I64TruncSatF64S | I64TruncSatF64U
            | TypedSelect { ty: ValType::F32 | ValType::F64 }
    )
}

/// An operator's name for a message: its variant, without operands.
fn name(op: &Operator<'_>) -> String {
    let debug = format!("{op:?}");
    debug.split([' ', '{', '(']).next().unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    // ---- Building modules ------------------------------------------------------------------

    /// WAT to Wasm, without custom sections: `wat` writes a `name` section whenever the text
    /// names anything, and uploads have none (`blockula program build` strips them).
    fn wat(src: &str) -> Vec<u8> {
        let wasm = wat::parse_str(src).unwrap_or_else(|e| panic!("bad test WAT: {e}\n{src}"));
        let mut out = wasm[..8].to_vec();
        let mut at = 8;
        while at < wasm.len() {
            let mut reader = BinaryReader::new(&wasm[at + 1..], at + 1);
            let len = reader.read_var_u32().unwrap() as usize;
            let end = reader.original_position() + len;
            if wasm[at] != 0 {
                out.extend_from_slice(&wasm[at..end]);
            }
            at = end;
        }
        out
    }

    /// A module of `body` and the one memory a program must have (last: WAT wants imports first).
    fn program(body: &str) -> Vec<u8> {
        wat(&format!(r#"(module {body} (memory (export "memory") 1 1))"#))
    }

    fn leb(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }

    fn section(id: u8, content: &[u8]) -> Vec<u8> {
        let mut s = vec![id];
        leb(content.len() as u64, &mut s);
        s.extend_from_slice(content);
        s
    }

    fn module(sections: &[Vec<u8>]) -> Vec<u8> {
        let mut m = b"\0asm\x01\0\0\0".to_vec();
        for s in sections {
            m.extend_from_slice(s);
        }
        m
    }

    /// Type 0 is `[] -> []`; one function of that type; one 1-page memory exported as `memory`.
    fn types() -> Vec<u8> {
        section(1, &[1, 0x60, 0, 0])
    }
    fn funcs() -> Vec<u8> {
        section(3, &[1, 0])
    }
    fn memory() -> Vec<u8> {
        section(5, &[1, 1, 1, 1])
    }
    fn export_memory() -> Vec<u8> {
        section(7, &[1, 6, b'm', b'e', b'm', b'o', b'r', b'y', 2, 0])
    }
    /// A code section with one body: local groups, then operators (an `end` is appended).
    fn code(locals: &[(u32, u8)], ops: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        leb(locals.len() as u64, &mut body);
        for (count, ty) in locals {
            leb(u64::from(*count), &mut body);
            body.push(*ty);
        }
        body.extend_from_slice(ops);
        body.push(0x0b);
        let mut content = vec![1];
        leb(body.len() as u64, &mut content);
        content.extend(body);
        section(10, &content)
    }

    /// The smallest valid program, by hand: `[] -> []` doing nothing, and the memory.
    fn minimal_bytes() -> Vec<u8> {
        module(&[types(), funcs(), memory(), export_memory(), code(&[], &[])])
    }

    fn refused(wasm: &[u8], rule: Rule) -> ValidateError {
        match validate(wasm) {
            Ok(info) => panic!("accepted a module breaking {rule:?}: {info:?}"),
            Err(e) => {
                assert_eq!(e.rule, rule, "wrong rule: {e}");
                e
            }
        }
    }

    // ---- Accepted programs ----------------------------------------------------------------

    const MINIMAL: &str = r#"(module
      (import "blockula" "input_len_v1" (func $input_len (result i32)))
      (import "blockula" "return_data_v1" (func $return_data (param i32 i32)))
      (memory (export "memory") 1 1)
      (data (i32.const 0) "hi")
      (func (export "bk_echo")
        (call $return_data (i32.const 0) (call $input_len))))"#;

    /// Every kind of thing a program may contain: imports, a table and its elements, globals,
    /// data, loops, `br_table`, `call_indirect`, `select`, loads and stores, `memory.size`.
    const RICH: &str = r#"(module
      (type $unary (func (param i64) (result i64)))
      (import "blockula" "input_len_v1" (func $input_len (result i32)))
      (import "blockula" "input_read_v1" (func $input_read (param i32)))
      (import "blockula" "storage_read_v1" (func $sread (param i32 i32 i32 i32) (result i32)))
      (import "blockula" "storage_write_v1" (func $swrite (param i32 i32 i32 i32 i32)))
      (import "blockula" "value_v1" (func $value (result i64)))
      (import "blockula" "transfer_v1" (func $transfer (param i32 i64 i32) (result i32)))
      (import "blockula" "abort_v1" (func $abort (param i32 i32 i32)))
      (import "blockula" "return_data_v1" (func $return_data (param i32 i32)))
      (memory (export "memory") 32 32)
      (table 4 4 funcref)
      (elem (i32.const 0) $double $square $double)
      (global $sp (mut i32) (i32.const 65536))
      (global $k i64 (i64.const 7))
      (data (i32.const 1024) "counter")
      (func $double (type $unary) (i64.shl (local.get 0) (i64.const 1)))
      (func $square (type $unary) (i64.mul (local.get 0) (local.get 0)))
      (func $sum (param $n i32) (result i64) (local $i i32) (local $acc i64)
        (block $done (loop $next
          (br_if $done (i32.ge_u (local.get $i) (local.get $n)))
          (local.set $acc (i64.add (local.get $acc) (i64.extend_i32_u (local.get $i))))
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br $next)))
        (local.get $acc))
      (func (export "bk_increment") (local $len i32) (local $v i64)
        (local.set $len (call $sread (i32.const 1024) (i32.const 7) (i32.const 2048) (i32.const 8)))
        (if (i32.eq (local.get $len) (i32.const 8))
          (then (local.set $v (i64.load (i32.const 2048))))
          (else (local.set $v (i64.const 0))))
        (i64.store (i32.const 2048) (i64.add (local.get $v) (global.get $k)))
        (call $swrite (i32.const 1024) (i32.const 7) (i32.const 2048) (i32.const 8) (i32.const 0)))
      (func (export "bk_dispatch") (local $sel i32)
        (call $input_read (i32.const 0))
        (local.set $sel (i32.load8_u (i32.const 0)))
        (block $c (block $b (block $a
          (br_table $a $b $c (local.get $sel)))
          (i64.store (i32.const 8) (call_indirect (type $unary) (i64.const 3) (i32.const 0)))
          (br $c))
          (i64.store (i32.const 8) (call $sum (i32.const 10))))
        (drop (select (i32.const 1) (i32.const 2) (local.get $sel)))
        (drop (memory.size))
        (global.set $sp (i32.sub (global.get $sp) (i32.const 16)))
        (if (i32.eqz (call $input_len)) (then (call $abort (i32.const 1) (i32.const 0) (i32.const 0))))
        (drop (call $transfer (i32.const 0) (call $value) (i32.const 0)))
        (call $return_data (i32.const 8) (i32.const 8))))"#;

    #[test]
    fn accepts_a_minimal_program() {
        let info = validate(&wat(MINIMAL)).unwrap();
        assert_eq!(
            info,
            ModuleInfo { exports: vec!["echo".into()], pages: 1, data_bytes: 2, imports: vec!["input_len_v1", "return_data_v1"] }
        );
        let bare = validate(&minimal_bytes()).unwrap();
        assert_eq!(bare, ModuleInfo { exports: vec![], pages: 1, data_bytes: 0, imports: vec![] });
    }

    #[test]
    fn accepts_everything_a_program_may_use() {
        let info = validate(&wat(RICH)).unwrap();
        assert_eq!(info.exports, ["dispatch", "increment"]);
        assert_eq!((info.pages, info.data_bytes), (32, 7));
        assert_eq!(info.imports.len(), 8);
        assert_eq!(info.imports[0], "input_len_v1");
    }

    // ---- One refusal per rule -------------------------------------------------------------

    #[test]
    fn refuses_code_over_the_size_limit() {
        let mut wasm = minimal_bytes();
        wasm.resize(config::MAX_CODE_BYTES + 1, 0);
        let e = refused(&wasm, Rule::Size);
        assert_eq!(e.offset, None);
        // At the limit it's parsed (and this one is malformed: trailing zeros are custom sections
        // of length 0 with no name byte).
        wasm.truncate(config::MAX_CODE_BYTES);
        assert_ne!(validate(&wasm).unwrap_err().rule, Rule::Size);
    }

    #[test]
    fn refuses_malformed_bytes() {
        refused(b"", Rule::Wasm);
        refused(b"\0asm", Rule::Wasm);
        refused(b"\0asm\x02\0\0\0", Rule::Wasm);
        refused(b"not wasm at all", Rule::Wasm);
        let mut truncated = minimal_bytes();
        truncated.pop();
        refused(&truncated, Rule::Wasm);
        // A component: refused at the header (the component model isn't compiled in).
        let e = validate(b"\0asm\x0d\0\x01\0").unwrap_err();
        assert!(matches!(e.rule, Rule::Wasm | Rule::Feature), "{e}");
    }

    /// Float-to-int conversions and reinterprets in unreachable code: wasmparser's validator
    /// accepts them with floats off (they pop a float from the polymorphic stack without calling
    /// `check_floats_enabled`), so the operator walk must refuse them.
    #[test]
    fn refuses_unreachable_float_conversions() {
        for (op, opcode) in [
            ("i32.trunc_f32_s", 0xa8),
            ("i32.trunc_f64_u", 0xab),
            ("i64.trunc_f32_u", 0xaf),
            ("i64.trunc_f64_s", 0xb0),
            ("i32.reinterpret_f32", 0xbc),
            ("i64.reinterpret_f64", 0xbd),
        ] {
            let wasm = program(&format!(r#"(func) (func (export "bk_x") unreachable {op} drop)"#));
            let gap = Validator::new_with_features(FEATURES).validate_all(&wasm).map(drop);
            assert!(gap.is_ok(), "wasmparser now refuses {op} itself: {gap:?}");
            let e = refused(&wasm, Rule::Float);
            assert_eq!(e.func, Some(1), "{e}");
            assert_eq!(wasm[e.offset.unwrap()], opcode, "{op}: {e}");
            assert!(e.to_string().contains("(function 1, at offset 0x"), "{e}");
        }
        // The saturating truncations: wasmparser refuses these itself (their proposal is off);
        // the walk gets there first and calls them floats.
        for op in ["i32.trunc_sat_f32_s", "i64.trunc_sat_f64_u"] {
            let wasm = program(&format!(r#"(func (export "bk_x") unreachable {op} drop)"#));
            assert!(Validator::new_with_features(FEATURES).validate_all(&wasm).is_err());
            let e = refused(&wasm, Rule::Float);
            assert_eq!((e.func, wasm[e.offset.unwrap()]), (Some(0), 0xfc), "{e}");
        }
        // Also reachable conversions of integers to floats, and constants, behind a branch.
        for op in ["f32.convert_i32_s", "f64.promote_f32", "f32.reinterpret_i32"] {
            let wasm = program(&format!(r#"(func (export "bk_x") (block br 0 i32.const 0 {op} drop))"#));
            refused(&wasm, Rule::Float);
        }
    }

    #[test]
    fn refuses_float_types_anywhere() {
        let cases = [
            r#"(func (param f32))"#,                                  // a signature
            r#"(func (result f64) unreachable)"#,                     // a result
            r#"(func (local f64))"#,                                  // a local
            r#"(global f32 (f32.const 0))"#,                          // a global
            r#"(func (block (result f32) unreachable) drop)"#,        // a block type
            r#"(func f32.const 1 drop)"#,                             // a constant
            r#"(func i32.const 0 f64.load drop)"#,                    // a load
            r#"(type (func (param i32 f64)))"#,                       // an unused type
        ];
        for case in cases {
            refused(&program(case), Rule::Float);
        }
        // A local's error names the function and the local group's offset.
        let e = refused(&program(r#"(func) (func (local i32) (local f64))"#), Rule::Float);
        assert_eq!(e.func, Some(1));
    }

    #[test]
    fn refuses_everything_outside_wasm1() {
        let cases = [
            r#"(func i32.const 0 i32.extend8_s drop)"#,                                 // sign extension
            r#"(func (memory.fill (i32.const 0) (i32.const 0) (i32.const 0)))"#,       // bulk memory
            r#"(func return_call 0)"#,                                                  // tail calls
            r#"(func (drop (select (result i32) (i32.const 0) (i32.const 1) (i32.const 1))))"#, // typed select
            r#"(func (result i32 i32) unreachable)"#,                                   // multi-value
            r#"(func (block (param i32) drop) )"#,                                      // multi-value block
            r#"(func ref.null func drop)"#,                                             // reference types
            r#"(table 1 1 funcref) (func (drop (table.size)))"#,                        // reference types
            r#"(global i32 (i32.add (i32.const 1) (i32.const 2)))"#,                   // extended const
            r#"(global $a i32 (i32.const 1)) (global i32 (global.get $a))"#,          // global.get of a defined global
            r#"(data "passive")"#,                                                      // a passive segment
        ];
        for case in cases {
            refused(&program(case), Rule::Feature);
        }
        // The data count section (bulk memory), by hand: wasmparser accepts it with bulk memory off.
        let data = section(11, &[1, 0, 0x41, 0, 0x0b, 1, b'x']);
        let wasm = module(&[types(), funcs(), memory(), export_memory(), section(12, &[1]), code(&[], &[0x01; 8]), data]);
        refused(&wasm, Rule::Feature);
        // A data segment naming memory 0 explicitly (flags 2) and a passive one (flags 1), by hand.
        for segment in [&[1u8, 2, 0, 0x41, 0, 0x0b, 1, b'x'][..], &[1, 1, 1, b'x']] {
            let wasm = module(&[types(), funcs(), memory(), export_memory(), code(&[], &[0x01; 8]), section(11, segment)]);
            refused(&wasm, Rule::Feature);
        }
        // An element segment naming table 0 explicitly (flags 2), by hand.
        let table = section(4, &[1, 0x70, 1, 1, 1]);
        let elem = section(9, &[1, 2, 0, 0x41, 0, 0x0b, 0, 1, 0]);
        let wasm = module(&[types(), funcs(), table, memory(), export_memory(), elem, code(&[], &[])]);
        refused(&wasm, Rule::Feature);
    }

    #[test]
    fn refuses_imports_other_than_abi_v1_host_functions() {
        let cases = [
            r#"(import "env" "input_len_v1" (func (result i32)))"#,            // another module
            r#"(import "blockula" "input_len_v2" (func (result i32)))"#,       // not in ABI v1
            r#"(import "blockula" "input_len_v1" (func (result i64)))"#,       // the wrong result
            r#"(import "blockula" "transfer_v1" (func (param i32 i32 i32) (result i32)))"#, // the wrong params
            r#"(import "blockula" "abort_v1" (func (param i32 i32 i32) (result i32)))"#, // an extra result
            r#"(import "blockula" "mem" (memory 1 1))"#,                       // a memory
            r#"(import "blockula" "t" (table 1 funcref))"#,                    // a table
            r#"(import "blockula" "g" (global i32))"#,                         // a global
            r#"(import "blockula" "value_v1" (func (result i64)))
               (import "blockula" "value_v1" (func (result i64)))"#,           // twice
        ];
        for case in cases {
            refused(&program(case), Rule::Import);
        }
        // Every host function, with its signature, is accepted.
        let wasm_ty = |t: &abi::Ty| match t {
            abi::Ty::I32 => "i32",
            abi::Ty::I64 => "i64",
        };
        let imports: String = abi::HOST_FNS
            .iter()
            .map(|h| {
                let params: Vec<_> = h.params.iter().map(wasm_ty).collect();
                let results: Vec<_> = h.results.iter().map(wasm_ty).collect();
                format!(r#"(import "blockula" "{}" (func (param {}) (result {})))"#, h.name, params.join(" "), results.join(" "))
            })
            .collect();
        let info = validate(&program(&imports)).unwrap();
        assert_eq!(info.imports, abi::HOST_FNS.iter().map(|h| h.name).collect::<Vec<_>>());
    }

    #[test]
    fn refuses_any_memory_but_one_fixed_exported_memory() {
        refused(&wat(r#"(module (func (export "bk_x")))"#), Rule::Memory); // none
        refused(&wat(r#"(module (memory 1 1))"#), Rule::Memory); // not exported
        for memory in [
            "(memory (export \"memory\") 1 2)",   // initial < maximum: it could grow
            "(memory (export \"memory\") 1)",     // no maximum
            "(memory (export \"memory\") 33 33)", // past MAX_PAGES
            "(memory (export \"memory\") 1 1 shared)",
            "(memory (export \"memory\") i64 1 1)",
            "(memory (export \"memory\") 1 1 (pagesize 1))",
            "(memory (export \"memory\") 1 1) (memory 1 1)", // two
        ] {
            refused(&wat(&format!("(module {memory})")), Rule::Memory);
        }
        let at_limit = validate(&wat(&format!(r#"(module (memory (export "memory") {0} {0}))"#, config::MAX_PAGES))).unwrap();
        assert_eq!(at_limit.pages, config::MAX_PAGES);
    }

    /// `memory.grow` is Wasm 1.0, so wasmparser and the engine accept it; `validate` refuses it
    /// anywhere in a body, reachable or not, by 0 pages too, naming the function and the opcode's
    /// offset. `memory.size` stays allowed.
    #[test]
    fn refuses_memory_grow_reachable_or_not() {
        for body in [
            "(drop (memory.grow (i32.const 1)))",                             // reachable
            "(drop (memory.grow (i32.const 0)))",                             // by 0 pages
            "(drop (memory.grow (i32.const -1)))",                            // by u32::MAX pages
            "(if (i32.const 0) (then (drop (memory.grow (i32.const 0)))))", // behind a branch never taken
            "unreachable (drop (memory.grow (i32.const 1)))",                 // unreachable code
            "unreachable memory.grow drop",                                   // unreachable, with no operand of its own
            "(block br 0 (drop (memory.grow (i32.const 0))))",                // after a branch
        ] {
            let wasm = program(&format!(r#"(func) (func (export "bk_x") {body})"#));
            let wasmparser = Validator::new_with_features(FEATURES).validate_all(&wasm).map(drop);
            assert!(wasmparser.is_ok(), "wasmparser now refuses {body} itself: {wasmparser:?}");
            assert_eq!(engine(&wasm), Ok(()), "{body}");
            let e = refused(&wasm, Rule::MemoryGrow);
            assert_eq!(e.func, Some(1), "{body}: {e}");
            let at = e.offset.unwrap();
            assert_eq!(&wasm[at..at + 2], &[0x40u8, 0x00][..], "{body}: the opcode and its memory byte: {e}");
            let shown = e.to_string();
            assert!(shown.starts_with("a memory.grow") && shown.ends_with(&format!("(function 1, at offset {at:#x})")), "{shown}");
        }
        // Function indices count imports first: with two imported, the one body is function 2.
        let wasm = program(
            r#"(import "blockula" "value_v1" (func (result i64))) (import "blockula" "input_len_v1" (func $len (result i32)))
               (func (export "bk_x") (drop (memory.size)) (drop (memory.grow (call $len))))"#,
        );
        let e = refused(&wasm, Rule::MemoryGrow);
        assert_eq!((e.func, wasm[e.offset.unwrap()]), (Some(2), 0x40), "{e}");
        // `memory.size`, reachable or not, passes.
        validate(&program(r#"(func (export "bk_x") (drop (memory.size)) unreachable memory.size drop)"#)).unwrap();
    }

    #[test]
    fn refuses_tables_past_one_or_the_entry_limit() {
        let max = config::MAX_TABLE_ENTRIES;
        refused(&program("(table 1 1 funcref) (table 1 1 funcref)"), Rule::Table);
        refused(&program(&format!("(table {} {} funcref)", max + 1, max + 1)), Rule::Table);
        refused(&program(&format!("(table 1 {} funcref)", max + 1)), Rule::Table);
        refused(&program("(table 1 1 externref)"), Rule::Table);
        validate(&program(&format!("(table {max} {max} funcref)"))).unwrap();
    }

    #[test]
    fn refuses_counts_past_their_limits() {
        // Functions: one past the limit, refused at the function section before the code. (No
        // module that passes reaches the limit: 10,001 bodies averaging 40 bytes exceed
        // MAX_CODE_BYTES, so the size and average-body rules bound functions at about 6,500.)
        let e = refused(&program(&"(func)".repeat(config::MAX_FUNCS as usize + 1)), Rule::Limit);
        assert!(e.message.contains("10001 functions"), "{e}");
        // Locals, parameters included.
        let locals = |params: usize, locals: usize| {
            program(&format!("(func (param {}) (local {}))", "i32 ".repeat(params), "i64 ".repeat(locals)))
        };
        validate(&locals(4, config::MAX_LOCALS as usize - 4)).unwrap();
        let e = refused(&locals(4, config::MAX_LOCALS as usize - 3), Rule::Limit);
        assert_eq!(e.func, Some(0));
        // Globals.
        let globals = |n: u32| program(&"(global (mut i32) (i32.const 0))".repeat(n as usize));
        validate(&globals(config::MAX_GLOBALS)).unwrap();
        refused(&globals(config::MAX_GLOBALS + 1), Rule::Limit);
        // Parameters of a type.
        let params = |n: usize| program(&format!("(type (func (param {})))", "i32 ".repeat(n)));
        validate(&params(MAX_PARAMS)).unwrap();
        refused(&params(MAX_PARAMS + 1), Rule::Limit);
        // Data and element segments.
        let filler = format!("(func {})", "i64.const 0 drop ".repeat(1_100));
        let data = |n: u32| program(&format!("{filler}{}", r#"(data (i32.const 0) "x")"#.repeat(n as usize)));
        validate(&data(MAX_DATA_SEGMENTS)).unwrap();
        refused(&data(MAX_DATA_SEGMENTS + 1), Rule::Limit);
        let elems = |n: u32| program(&format!("(table 1 1 funcref) (func $f) {}", "(elem (i32.const 0) $f)".repeat(n as usize)));
        validate(&elems(MAX_ELEMENT_SEGMENTS)).unwrap();
        refused(&elems(MAX_ELEMENT_SEGMENTS + 1), Rule::Limit);
    }

    /// Local counts are u32s that can wrap a u32 sum: two groups of 2³¹ add up to 0.
    #[test]
    fn refuses_overflowing_local_counts() {
        const I32: u8 = 0x7f;
        for groups in [
            &[(u32::MAX, I32), (1, I32)][..],
            &[(1 << 31, I32), (1 << 31, I32)],
            &[(1, I32), (u32::MAX, I32)],
            &[(config::MAX_LOCALS, I32), (1, I32)],
        ] {
            let wasm = module(&[types(), funcs(), memory(), export_memory(), code(groups, &[])]);
            let e = refused(&wasm, Rule::Limit);
            assert_eq!(e.func, Some(0), "{e}");
        }
        // With 32 parameters, a group of u32::MAX - 31 locals makes exactly 2³².
        let params = section(1, &[&[1u8, 0x60, 32][..], &[0x7f; 32], &[0]].concat());
        let wasm = module(&[params, funcs(), memory(), export_memory(), code(&[(u32::MAX - 31, I32)], &[])]);
        refused(&wasm, Rule::Limit);
        let wasm = module(&[
            section(1, &[&[1u8, 0x60, 32][..], &[0x7f; 32], &[0]].concat()),
            funcs(),
            memory(),
            export_memory(),
            code(&[(config::MAX_LOCALS - 32, I32)], &[]),
        ]);
        validate(&wasm).unwrap();
    }

    #[test]
    fn refuses_data_larger_than_the_code_or_outside_memory() {
        let e = refused(&program(r#"(func) (data (i32.const 0) "more bytes of data than code")"#), Rule::Segment);
        assert!(e.message.contains("more than the code"), "{e}");
        let filler = format!("(func {})", "i64.const 0 drop ".repeat(20));
        refused(&program(&format!(r#"{filler} (data (i32.const 65535) "xy")"#)), Rule::Segment);
        refused(&program(&format!(r#"{filler} (data (i32.const -1) "x")"#)), Rule::Segment);
        validate(&program(&format!(r#"{filler} (data (i32.const 65534) "xy")"#))).unwrap();
        // Element segments must fit the table: it never grows.
        refused(&program("(table 2 2 funcref) (func $f) (elem (i32.const 1) $f $f)"), Rule::Segment);
        refused(&program("(func $f) (elem (i32.const 0) $f)"), Rule::Segment); // no table at all
        validate(&program("(table 2 2 funcref) (func $f) (elem (i32.const 0) $f $f)")).unwrap();
    }

    #[test]
    fn refuses_tiny_average_bodies() {
        // 400 empty bodies (3 bytes each with their size) pass 1,000 bytes and average 3.
        let wasm = program(&"(func)".repeat(400));
        let e = refused(&wasm, Rule::BodySize);
        assert!(e.message.contains("average 3 bytes"), "{e}");
        // Under 1,000 bytes the guard doesn't apply.
        validate(&program(&"(func)".repeat(300))).unwrap();
    }

    #[test]
    fn refuses_exports_other_than_memory_and_methods() {
        let cases = [
            r#"(func (export "run"))"#,                                // no bk_ prefix
            r#"(func (export "bk_"))"#,                                // empty method
            r#"(func (export "bk_Run"))"#,                             // not [a-z][a-z0-9_]*
            r#"(func (export "bk_1st"))"#,                             // starts with a digit
            r#"(func (export "bk_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"))"#, // 33 characters
            r#"(func (export "bk_x") (param i32))"#,                   // takes params
            r#"(func (export "bk_x") (result i32) i32.const 0)"#,      // returns a value
            r#"(global (export "bk_g") i32 (i32.const 0))"#,           // a global
            r#"(table (export "bk_t") 1 funcref)"#,                    // a table
            r#"(func (export "memory"))"#,                             // "memory" as a function
            r#"(import "blockula" "debug_log_v1" (func (param i32 i32)))
               (export "bk_log" (func 0))"#,                           // a host function
        ];
        for case in cases {
            let wasm = wat(&format!(r#"(module {case} (memory 1 1) (export "memory" (memory 0)))"#));
            refused(&wasm, Rule::Export);
        }
        // The memory under a second name.
        refused(&wat(r#"(module (memory (export "memory") (export "mem") 1 1))"#), Rule::Export);
        let max = "a".repeat(32);
        let info = validate(&program(&format!(r#"(func (export "bk_{max}")) (func (export "bk_a1_"))"#))).unwrap();
        assert_eq!(info.exports, ["a1_".to_string(), max]);
    }

    #[test]
    fn refuses_a_start_function() {
        let e = refused(&program("(func $init) (start $init)"), Rule::Start);
        assert!(e.offset.is_some());
    }

    #[test]
    fn refuses_custom_sections() {
        for (name, data) in [(&b"name"[..], &b"\x01\x02\x00\x00"[..]), (b"producers", b""), (b"", b"payload")] {
            let mut content = Vec::new();
            leb(name.len() as u64, &mut content);
            content.extend_from_slice(name);
            content.extend_from_slice(data);
            let mut wasm = minimal_bytes();
            let at = wasm.len();
            wasm.extend(section(0, &content));
            let e = refused(&wasm, Rule::CustomSection);
            assert_eq!(e.offset, Some(at + 2), "{e}");
        }
        // Before the first section, too.
        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        wasm.extend(section(0, b"\x03abc"));
        wasm.extend_from_slice(&minimal_bytes()[8..]);
        refused(&wasm, Rule::CustomSection);
        // Names are quoted, escaped and cut short in the message.
        let name = "\u{202e}x".repeat(50);
        let mut content = Vec::new();
        leb(name.len() as u64, &mut content);
        content.extend_from_slice(name.as_bytes());
        let mut wasm = minimal_bytes();
        wasm.extend(section(0, &content));
        let e = validate(&wasm).unwrap_err();
        assert!(e.message.contains("\\u{202e}") && e.message.contains('…') && e.message.len() < 600, "{e}");
        // wasmparser's own messages: control characters replaced, length bounded.
        assert_eq!(bounded("a\u{0085}b\nc"), "a?b?c");
        assert_eq!(bounded(&"x".repeat(4 * MAX_QUOTED)), "x".repeat(4 * MAX_QUOTED));
        assert_eq!(bounded(&"x".repeat(1_000)), "x".repeat(4 * MAX_QUOTED) + "…");
    }

    /// A section can't declare more entries than it has bytes: refused before wasmparser reserves
    /// storage for them.
    #[test]
    fn refuses_declared_counts_past_the_section() {
        let mut lying = vec![];
        leb(1_000_000, &mut lying);
        lying.extend([0x60, 0, 0]);
        let e = refused(&module(&[section(1, &lying)]), Rule::Wasm);
        assert!(e.message.contains("declares 1000000 entries"), "{e}");
        let mut lying = vec![];
        leb(u64::from(u32::MAX), &mut lying);
        refused(&module(&[types(), section(3, &lying)]), Rule::Wasm);
    }

    // ---- Engines --------------------------------------------------------------------------

    /// soroban-wasmi 29 under PROGRAMS.md 4.2, as `engine/soroban.rs` maps it, restated here so the
    /// test pins the settings (fuel costs don't reach translation).
    fn soroban_module_new(wasm: &[u8]) -> Result<(), String> {
        use soroban_wasmi::{Config, Engine, FuelConsumptionMode, FuelCosts, Module, StackLimits};
        let mut costs = FuelCosts::default();
        (costs.base, costs.entity, costs.load, costs.store, costs.call) = (1, 1, 1, 1, 10);
        let mut config = Config::default();
        config
            .consume_fuel(true)
            .fuel_consumption_mode(FuelConsumptionMode::Eager)
            .set_fuel_costs(costs)
            .floats(false)
            .wasm_mutable_global(true)
            .wasm_sign_extension(false)
            .wasm_saturating_float_to_int(false)
            .wasm_multi_value(false)
            .wasm_bulk_memory(false)
            .wasm_reference_types(false)
            .wasm_tail_call(false)
            .wasm_extended_const(false)
            .set_stack_limits(StackLimits::new(8_192, 131_072, 1_024).unwrap())
            .set_cached_stacks(2);
        Module::new(&Engine::new(&config), wasm).map(drop).map_err(|e| e.to_string())
    }

    /// What the engine's `Module::new` says, a panic included.
    fn engine(wasm: &[u8]) -> Result<(), String> {
        catch_unwind(AssertUnwindSafe(|| soroban_module_new(wasm))).unwrap_or_else(|_| Err("PANIC".into()))
    }

    #[test]
    fn accepted_programs_pass_the_engine() {
        for wasm in [wat(MINIMAL), wat(RICH), minimal_bytes()] {
            validate(&wasm).unwrap();
            engine(&wasm).unwrap();
        }
    }

    /// Modules at each limit `validate` sets: every one must pass the engine, or a rule promises
    /// what the engine doesn't deliver.
    #[test]
    fn modules_at_the_limits_pass_the_engine() {
        let max = config::MAX_TABLE_ENTRIES;
        let mut cases = vec![
            ("pages", wat(&format!(r#"(module (memory (export "memory") {0} {0}))"#, config::MAX_PAGES))),
            ("table", program(&format!("(table {max} {max} funcref)"))),
            ("locals", program(&format!("(func (param {}) (local {}))", "i64 ".repeat(MAX_PARAMS), "i64 ".repeat(config::MAX_LOCALS as usize - MAX_PARAMS)))),
            ("globals", program(&"(global (mut i64) (i64.const -1))".repeat(config::MAX_GLOBALS as usize))),
            ("params", program(&format!("(func (export \"bk_x\") (call 1 {})) (func (param {}))", "(i32.const 0) ".repeat(MAX_PARAMS), "i32 ".repeat(MAX_PARAMS)))),
        ];
        let filler = format!("(func {})", "i64.const 0 drop ".repeat(1_100));
        cases.push(("data segments", program(&format!("{filler}{}", r#"(data (i32.const 0) "x")"#.repeat(MAX_DATA_SEGMENTS as usize)))));
        cases.push(("element segments", program(&format!("(table 1 1 funcref) (func $f) {}", "(elem (i32.const 0) $f)".repeat(MAX_ELEMENT_SEGMENTS as usize)))));
        // As many functions as fit the size limit at the smallest average body.
        let func = format!("(func {})", "i64.const 0 i64.const 0 i64.add drop ".repeat(7));
        cases.push(("functions", program(&func.repeat(5_000))));
        for (what, wasm) in cases {
            validate(&wasm).unwrap_or_else(|e| panic!("{what}: {e}"));
            engine(&wasm).unwrap_or_else(|e| panic!("{what}: {e}"));
        }
    }

    /// What makes a single function large for a translator: a deep operand stack, many distinct
    /// constants, deep block nesting, a wide `br_table`. Prints what the engine says; whatever
    /// validate accepts, it must.
    #[test]
    fn large_functions_and_the_engine() {
        let size = |n: usize, per: usize| n.min((config::MAX_CODE_BYTES - 200) / per);
        let deep_stack = |n| program(&format!("(func {}{})", "i32.const 0 ".repeat(n), "drop ".repeat(n)));
        let constants = |n: usize| {
            program(&format!(
                "(func (param i64) (result i64) {} local.get 0)",
                (0..n).map(|i| format!("local.get 0 i64.const {} i64.add local.set 0 ", 1_000_000 + i)).collect::<String>()
            ))
        };
        let nesting = |n| program(&format!("(func {}{})", "block ".repeat(n), "end ".repeat(n)));
        let br_table = |n| program(&format!("(func (block (br_table {} (i32.const 0))))", "0 ".repeat(n)));
        let cases = [
            ("operand stack 1,000", deep_stack(1_000)),
            ("operand stack at the limit", deep_stack(MAX_OPERAND_STACK as usize)),
            ("operand stack 30,000", deep_stack(30_000)),
            ("operand stack 70,000", deep_stack(size(70_000, 3))),
            ("constants 1,000", constants(1_000)),
            ("constants 25,000", constants(size(25_000, 10))),
            ("nesting 50,000", nesting(size(50_000, 3))),
            ("br_table 100,000", br_table(size(100_000, 1))),
        ];
        let mut refused_by_the_engine = Vec::new();
        for (what, wasm) in cases {
            let ours = validate(&wasm).map(drop).map_err(|e| e.to_string());
            let soroban = engine(&wasm);
            println!("{what} ({} bytes): validate {ours:?}, soroban-wasmi {soroban:?}", wasm.len());
            if ours.is_ok() && soroban.is_err() {
                refused_by_the_engine.push(what);
            }
        }
        assert!(refused_by_the_engine.is_empty(), "validate accepts what the engine refuses: {refused_by_the_engine:?}");
    }

    /// soroban-wasmi's parser (wasmparser-nostd 0.100.2, `read_memarg`) always decodes the
    /// multi-memory memarg form, so it accepts `i32.load` whose flags set bit 6 and name memory 0,
    /// with multi-memory off. Wasm 1.0 has no such encoding: wasmparser 0.228 refuses it as
    /// "alignment too large", and so does `validate`.
    #[test]
    fn refuses_the_multi_memory_memarg_soroban_wasmi_accepts() {
        let ops = [0x41, 0x00, 0x28, 0x42, 0x00, 0x00, 0x1a]; // i32.const 0, i32.load (flags 0x42, memory 0, offset 0), drop
        let wasm = module(&[types(), funcs(), memory(), export_memory(), code(&[], &ops)]);
        let e = refused(&wasm, Rule::Wasm);
        assert!(e.message.contains("alignment too large"), "{e}");
        assert_eq!(engine(&wasm), Ok(()));
    }

    // ---- Never panics, and agrees with the engine ------------------------------------------

    /// xorshift64*: a fixed-seed generator, so every run mutates the same way.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }
        fn below(&mut self, n: usize) -> usize {
            if n == 0 { 0 } else { (self.next() % n as u64) as usize }
        }
    }

    /// The modules mutations start from: accepted programs, and refused ones of each kind.
    fn seeds() -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
        let mut valid = vec![wat(MINIMAL), wat(RICH), minimal_bytes()];
        for case in [
            r#"(global (mut i32) (i32.const 0)) (global i64 (i64.const 1)) (func (export "bk_g") (global.set 0 (i32.wrap_i64 (global.get 1))))"#,
            r#"(func (param i32 i64) (result i64) (local i32) (if (result i64) (local.get 0) (then (local.get 1)) (else (i64.const 0)))) (func (export "bk_x") (drop (call 0 (i32.const 1) (i64.const 2))))"#,
            r#"(table 3 3 funcref) (elem (i32.const 1) 0 1) (type $t (func (param i32) (result i32))) (func (type $t) (i32.mul (local.get 0) (i32.const 3))) (func (type $t) (i32.rotl (local.get 0) (i32.const 5))) (func (export "bk_i") (drop (call_indirect (type $t) (i32.const 7) (i32.const 2))))"#,
            r#"(func (export "bk_loop") (local i64) (loop (local.set 0 (i64.add (local.get 0) (i64.const 1))) (br_if 0 (i64.lt_u (local.get 0) (i64.const 10)))) (i64.store32 offset=8 align=2 (i32.const 0) (local.get 0)))"#,
            r#"(import "blockula" "call_v1" (func (param i32 i32 i32 i32 i32 i32 i64 i64) (result i32))) (import "blockula" "emit_v1" (func (param i32 i32 i32 i32))) (func (export "bk_c") (drop (call 0 (i32.const 0) (i32.const 32) (i32.const 64) (i32.const 3) (i32.const 0) (i32.const 0) (i64.const 0) (i64.const -1))) (call 1 (i32.const 0) (i32.const 1) (i32.const 0) (i32.const 0)))"#,
            r#"(func (export "bk_t") (block (block (block (br_table 0 1 2 (i32.load16_s (i32.const 2)))) unreachable) nop)) (data (i32.const 100) "abc")"#,
        ] {
            valid.push(program(case));
        }
        let mut invalid = Vec::new();
        for case in [
            r#"(func (export "bk_x") unreachable i32.trunc_f32_s drop)"#,
            r#"(func (local f64))"#,
            r#"(func i32.const 0 i32.extend8_s drop)"#,
            r#"(import "blockula" "input_len_v1" (func (result i64)))"#,
            r#"(table 2 2 funcref) (func $f) (elem (i32.const 1) $f $f)"#,
            r#"(func $init) (start $init)"#,
            r#"(func (export "bk_x") (param i32))"#,
            r#"(func) (data (i32.const 0) "more data than code, by a few bytes")"#,
            r#"(func (export "bk_g") (drop (memory.grow (i32.const 0))))"#,
        ] {
            invalid.push(program(case));
        }
        let data = section(11, &[1, 0, 0x41, 0, 0x0b, 1, b'x']);
        invalid.push(module(&[types(), funcs(), memory(), export_memory(), section(12, &[1]), code(&[], &[0x01; 8]), data]));
        invalid.push(module(&[types(), funcs(), memory(), export_memory(), code(&[(u32::MAX, 0x7f), (1, 0x7f)], &[])]));
        let mut custom = minimal_bytes();
        custom.extend(section(0, b"\x04name\x00"));
        invalid.push(custom);
        (valid, invalid)
    }

    /// A module's sections as `(id, content)`, if its framing parses.
    fn split(wasm: &[u8]) -> Option<Vec<(u8, Vec<u8>)>> {
        let mut sections = Vec::new();
        let mut reader = BinaryReader::new(wasm.get(8..)?, 8);
        while !reader.eof() {
            let id = reader.read_u8().ok()?;
            let len = reader.read_var_u32().ok()? as usize;
            sections.push((id, reader.read_bytes(len).ok()?.to_vec()));
        }
        Some(sections)
    }

    /// A code section's bodies, if its framing parses.
    fn bodies(content: &[u8]) -> Option<Vec<Vec<u8>>> {
        let mut reader = BinaryReader::new(content, 0);
        let count = reader.read_var_u32().ok()?;
        let bodies = (0..count).map(|_| {
            let len = reader.read_var_u32().ok()? as usize;
            Some(reader.read_bytes(len).ok()?.to_vec())
        });
        let bodies = bodies.collect::<Option<Vec<_>>>()?;
        reader.eof().then_some(bodies)
    }

    /// Mutates one byte of `bytes`: a bit flip, an interesting value, an insertion or a deletion.
    fn poke(rng: &mut Rng, bytes: &mut Vec<u8>) {
        const INTERESTING: [u8; 12] = [0x00, 0x01, 0x02, 0x0b, 0x40, 0x41, 0x60, 0x70, 0x7f, 0x80, 0xfc, 0xff];
        let at = rng.below(bytes.len() + 1);
        match rng.below(4) {
            0 if at < bytes.len() => bytes[at] ^= 1 << rng.below(8),
            1 if at < bytes.len() => bytes[at] = INTERESTING[rng.below(INTERESTING.len())],
            2 if at < bytes.len() => drop(bytes.remove(at)),
            _ => bytes.insert(at, if rng.below(2) == 0 { rng.next() as u8 } else { INTERESTING[rng.below(INTERESTING.len())] }),
        }
    }

    /// A structure-aware mutation: one byte inside one section, or inside one function body,
    /// with every length re-encoded, so far more mutants get past the framing.
    fn mutate_inside(rng: &mut Rng, wasm: &[u8]) -> Option<Vec<u8>> {
        let mut sections = split(wasm)?;
        let i = rng.below(sections.len());
        let (id, content) = sections.get_mut(i)?;
        match bodies(content).filter(|b| *id == 10 && !b.is_empty()) {
            Some(mut bodies) => {
                let j = rng.below(bodies.len());
                poke(rng, &mut bodies[j]);
                content.clear();
                leb(bodies.len() as u64, content);
                for body in bodies {
                    leb(body.len() as u64, content);
                    content.extend(body);
                }
            }
            None => poke(rng, content),
        }
        Some(module(&sections.iter().map(|(id, c)| section(*id, c)).collect::<Vec<_>>()))
    }

    /// One mutant: mostly of an accepted program (so enough survive to exercise the engines),
    /// mostly one mutation.
    fn mutate(rng: &mut Rng, valid: &[Vec<u8>], invalid: &[Vec<u8>]) -> Vec<u8> {
        const INTERESTING: [u8; 12] = [0x00, 0x01, 0x02, 0x0b, 0x40, 0x41, 0x60, 0x70, 0x7f, 0x80, 0xfc, 0xff];
        let pick = |rng: &mut Rng| if rng.below(4) == 0 { &invalid[rng.below(invalid.len())] } else { &valid[rng.below(valid.len())] };
        let mut m = pick(rng).clone();
        let rounds = [1, 1, 1, 1, 1, 1, 2, 2, 3, 4][rng.below(10)];
        for _ in 0..rounds {
            let at = rng.below(m.len() + 1);
            match rng.below(16) {
                0 if !m.is_empty() => {
                    let i = rng.below(m.len());
                    m[i] ^= 1 << rng.below(8);
                }
                1 if !m.is_empty() => {
                    let i = rng.below(m.len());
                    m[i] = INTERESTING[rng.below(INTERESTING.len())];
                }
                2 => m.truncate(at),
                3 => {
                    let other = pick(rng);
                    let from = rng.below(other.len() + 1);
                    m.truncate(at);
                    m.extend_from_slice(&other[from..]);
                }
                4 => {
                    let bytes: Vec<u8> = (0..1 + rng.below(8)).map(|_| rng.next() as u8).collect();
                    m.splice(at..at, bytes);
                }
                5 => {
                    let end = (at + 1 + rng.below(16)).min(m.len());
                    m.drain(at.min(end)..end);
                }
                6 if !m.is_empty() => {
                    let from = rng.below(m.len());
                    let chunk = m[from..(from + 1 + rng.below(32)).min(m.len())].to_vec();
                    m.splice(at..at, chunk);
                }
                7 => {
                    let big: &[u8] = [&[0xff, 0xff, 0xff, 0xff, 0x0f][..], &[0x80, 0x80, 0x80, 0x80, 0x00], &[0xe8, 0x07]][rng.below(3)];
                    let end = (at + big.len()).min(m.len());
                    m.splice(at..end, big.iter().copied());
                }
                _ => {
                    if let Some(inside) = mutate_inside(rng, &m) {
                        m = inside;
                    }
                }
            }
        }
        m
    }

    /// 500,000 mutants of valid and invalid modules (bit flips, interesting bytes, truncations,
    /// splices, insertions, deletions, duplications, huge LEBs, and byte edits inside one section
    /// or function body with the lengths re-encoded): `validate` returns on every one, and every
    /// one it accepts passes the engine's `Module::new`.
    #[test]
    fn never_panics_and_what_it_accepts_the_engine_accepts() {
        let (valid, invalid) = seeds();
        let mut rng = Rng(0x0b10_c0de_5eed_1234);
        let (mut accepted, mut disagreements) = (0, Vec::new());
        let mut rules = std::collections::BTreeMap::new();
        const CASES: u32 = 500_000;
        for case in 0..CASES {
            let wasm = mutate(&mut rng, &valid, &invalid);
            let Ok(result) = catch_unwind(AssertUnwindSafe(|| validate(&wasm))) else {
                panic!("validate panicked on case {case}: {wasm:02x?}");
            };
            match result {
                Ok(_) => {
                    accepted += 1;
                    if let Err(why) = engine(&wasm) {
                        disagreements.push(format!("case {case}: {why}\n  {wasm:02x?}"));
                    }
                }
                Err(e) => *rules.entry(format!("{:?}", e.rule)).or_insert(0) += 1,
            }
        }
        println!("accepted {accepted} of {CASES}; refused by rule: {rules:?}");
        assert!(accepted > 5_000, "too few mutants pass to exercise the engine: {accepted}");
        assert!(disagreements.is_empty(), "{} accepted modules the engine refused:\n{}", disagreements.len(), disagreements.join("\n"));
    }

    /// The engine on every mutant, accepted or not: what `validate` keeps from it. Counts engine
    /// panics and the modules the engine accepts that `validate` refuses (by rule; most are
    /// deliberate: custom sections, exports, imports, the memory). An experiment, not a gate:
    /// `cargo test engine_on_every_mutant -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn engine_on_every_mutant() {
        let (valid, invalid) = seeds();
        let mut rng = Rng(0x0b10_c0de_5eed_1234);
        let mut panics = Vec::new();
        let mut engine_only = std::collections::BTreeMap::new();
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        for case in 0..200_000 {
            let wasm = mutate(&mut rng, &valid, &invalid);
            let ours = validate(&wasm);
            let soroban = engine(&wasm);
            if soroban.as_ref().is_err_and(|e| e == "PANIC") {
                panics.push(format!("case {case} (validate: {:?}): {wasm:02x?}", ours.as_ref().map_err(|e| e.rule)));
            }
            if let Err(e) = &ours
                && soroban.is_ok()
            {
                *engine_only.entry(format!("{:?}", e.rule)).or_insert(0) += 1;
            }
        }
        std::panic::set_hook(hook);
        if let Some(first) = panics.first() {
            println!("soroban-wasmi panicked on {} mutants; first: {first}", panics.len());
        }
        println!("refused by validate, accepted by the engine: {engine_only:#?}");
        assert!(panics.iter().all(|c| !c.contains("validate: Ok")), "the engine panicked on a module validate accepts");
    }

    /// Validates every `.wasm` in `$BLOCKULA_VALIDATE_DIR` (built guests, say) and runs the engine
    /// on each. `cargo test validate_dir -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn validate_dir() {
        let dir = std::env::var("BLOCKULA_VALIDATE_DIR").expect("set BLOCKULA_VALIDATE_DIR");
        let mut paths: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
        paths.sort();
        for path in paths.into_iter().filter(|p| p.extension().is_some_and(|e| e == "wasm")) {
            let wasm = std::fs::read(&path).unwrap();
            println!("{}: {} bytes\n  validate: {:?}\n  soroban-wasmi: {:?}", path.display(), wasm.len(), validate(&wasm), engine(&wasm));
        }
    }
}
