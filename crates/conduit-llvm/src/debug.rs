//! Debug info (#50): DWARF, through LLVM's metadata, from where the
//! frontend says each of the module's sources is ([`Sources`]).
//!
//! Each function with a source in its body gets a `DISubprogram`, named
//! after its debug name, at the line of its first source, and every
//! instruction lowered from it a `DILocation`: that of the `let` or `jump`
//! it's lowered from, or of the last one before it with a source, so that
//! no call in a function with debug info goes without a location, which
//! LLVM's verifier rejects. Functions of the backend's own, like a
//! closure's entry or the runtime's, have none. Variables have none yet.

use std::collections::{BTreeMap, HashMap};

use conduit_ir::SourceId;

/// Where a source of the module is: what a frontend knows of one of its
/// `SourceId`s.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Location {
    /// The file, as a debugger should find it: absolute, or relative to
    /// [`Sources::directory`].
    pub file: Box<str>,
    /// Counted from 1.
    pub line: u32,
    /// Counted from 1, or 0 for none.
    pub column: u32,
}

/// Where the module's sources are, for debug info. With none, the module
/// has no debug info.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    /// The directory relative files are in, DWARF's compilation
    /// directory.
    pub directory: Box<str>,
    pub locations: BTreeMap<SourceId, Location>,
}

/// The metadata nodes made so far, numbered from 0.
pub(crate) struct Debug {
    sources: Sources,
    nodes: Vec<String>,
    unit: u32,
    files: HashMap<Box<str>, u32>,
    subroutine: u32,
    locations: HashMap<(u32, u32, u32), u32>,
}

impl Debug {
    /// The debug info of a module whose sources are `sources`, or none if
    /// there are none.
    pub(crate) fn new(sources: &Sources) -> Option<Debug> {
        let (_, first) = sources.locations.first_key_value()?;
        let first = first.file.clone();
        let mut d = Debug {
            sources: sources.clone(),
            nodes: Vec::new(),
            unit: 0,
            files: HashMap::new(),
            subroutine: 0,
            locations: HashMap::new(),
        };
        let file = d.file(&first);
        d.unit = d.node(format!(
            "distinct !DICompileUnit(language: DW_LANG_C, file: !{file}, producer: \"conduit\", \
             isOptimized: true, runtimeVersion: 0, emissionKind: FullDebug)"
        ));
        d.subroutine = d.node("!DISubroutineType(types: !{null})".into());
        Some(d)
    }

    fn node(&mut self, text: String) -> u32 {
        self.nodes.push(text);
        self.nodes.len() as u32 - 1
    }

    fn file(&mut self, name: &str) -> u32 {
        if let Some(&n) = self.files.get(name) {
            return n;
        }
        let text = format!(
            "!DIFile(filename: {}, directory: {})",
            string(name),
            string(&self.sources.directory.clone())
        );
        let n = self.node(text);
        self.files.insert(name.into(), n);
        n
    }

    /// Where `source` is, if the frontend said.
    pub(crate) fn location_of(&self, source: SourceId) -> Option<Location> {
        self.sources.locations.get(&source).cloned()
    }

    /// A `DISubprogram` for the function `linkage`, shown as `name`, at
    /// `at`.
    pub(crate) fn subprogram(&mut self, name: &str, linkage: &str, at: &Location) -> u32 {
        let file = self.file(&at.file);
        let (unit, ty) = (self.unit, self.subroutine);
        self.node(format!(
            "distinct !DISubprogram(name: {}, linkageName: {}, scope: !{file}, file: !{file}, \
             line: {line}, type: !{ty}, scopeLine: {line}, \
             spFlags: DISPFlagDefinition | DISPFlagOptimized, unit: !{unit})",
            string(name),
            string(linkage),
            line = at.line
        ))
    }

    /// A `DILocation` at `at`, in the function of `subprogram`.
    pub(crate) fn location(&mut self, subprogram: u32, at: &Location) -> u32 {
        let key = (subprogram, at.line, at.column);
        if let Some(&n) = self.locations.get(&key) {
            return n;
        }
        let n = self.node(format!(
            "!DILocation(line: {}, column: {}, scope: !{subprogram})",
            at.line, at.column
        ));
        self.locations.insert(key, n);
        n
    }

    /// The metadata, as lines of the module: the nodes, then the named
    /// ones LLVM looks for.
    pub(crate) fn finish(mut self) -> Vec<String> {
        let version = self.node("!{i32 7, !\"Dwarf Version\", i32 5}".into());
        let info = self.node("!{i32 2, !\"Debug Info Version\", i32 3}".into());
        let mut lines: Vec<String> = (self.nodes.iter().enumerate())
            .map(|(i, text)| format!("!{i} = {text}"))
            .collect();
        lines.push(format!("!llvm.dbg.cu = !{{!{}}}", self.unit));
        lines.push(format!("!llvm.module.flags = !{{!{version}, !{info}}}"));
        lines
    }
}

/// `s` as a metadata string: quotes, backslashes and what isn't printable
/// ASCII escaped as `\XX`.
fn string(s: &str) -> String {
    let mut out = String::from("\"");
    for b in s.bytes() {
        if b == b'"' || b == b'\\' || !(0x20..0x7f).contains(&b) {
            out.push_str(&format!("\\{b:02X}"));
        } else {
            out.push(b as char);
        }
    }
    out.push('"');
    out
}
