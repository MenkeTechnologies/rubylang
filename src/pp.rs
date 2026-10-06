//! `Kernel#pp` / `#pretty_inspect`: a port of MRI's `prettyprint.rb` (the
//! Oppen-style line breaker) and the `pretty_print` methods `pp.rb` defines for
//! the core classes.
//!
//! [`PrettyPrint`] is `PrettyPrint` line for line: text and breakables are
//! buffered until the pending line would overflow `maxwidth`, at which point the
//! OUTERMOST group that still holds a breakable is broken (every breakable of a
//! broken group becomes a newline plus the group's indent). [`Printer::pp`] is
//! `PP::PPMethods#pp` with the per-class `pretty_print` / `pretty_print_cycle`
//! bodies of `pp.rb` inlined: Array, Hash, Set, Struct, Data, Range, String,
//! MatchData and `Object#pretty_print`'s `pp_object` (sorted instance
//! variables). Every other value is a leaf printed through its `inspect`, which
//! is what `Object#pretty_print` does for a class whose `inspect` is not
//! Kernel's.
//!
//! Not ported: a user-defined `pretty_print(q)` (it needs a Ruby-visible `PP`
//! object), so such an object prints through its `inspect`.

use crate::builtins::{defines_own, inspect_of};
use crate::host::{with_host, RANGE_BEGINLESS, RANGE_ENDLESS};
use fusevm::Value;
use std::collections::VecDeque;

/// One buffered item: `PrettyPrint::Text` or `PrettyPrint::Breakable`.
enum Item {
    Text {
        text: String,
        width: usize,
    },
    Breakable {
        sep: String,
        width: usize,
        indent: usize,
        group: usize,
    },
}

impl Item {
    fn width(&self) -> usize {
        match self {
            Item::Text { width, .. } | Item::Breakable { width, .. } => *width,
        }
    }
}

/// `PrettyPrint::Group`: its breakables are only ever pushed, shifted and
/// tested for emptiness, so a count stands in for the list.
struct Group {
    depth: usize,
    pending: usize,
    broken: bool,
}

pub struct PrettyPrint {
    output: String,
    maxwidth: usize,
    output_width: usize,
    buffer_width: usize,
    buffer: VecDeque<Item>,
    groups: Vec<Group>,
    group_stack: Vec<usize>,
    /// `GroupQueue`: group ids by depth.
    queue: Vec<Vec<usize>>,
    indent: usize,
}

impl PrettyPrint {
    pub fn new(maxwidth: usize) -> Self {
        let mut q = PrettyPrint {
            output: String::new(),
            maxwidth,
            output_width: 0,
            buffer_width: 0,
            buffer: VecDeque::new(),
            groups: vec![Group {
                depth: 0,
                pending: 0,
                broken: false,
            }],
            group_stack: vec![0],
            queue: Vec::new(),
            indent: 0,
        };
        q.enq(0);
        q
    }

    fn enq(&mut self, g: usize) {
        let depth = self.groups[g].depth;
        while self.queue.len() <= depth {
            self.queue.push(Vec::new());
        }
        self.queue[depth].push(g);
    }

    /// `GroupQueue#deq`: the outermost (then latest) group still holding a
    /// breakable, marked broken; groups passed over at that depth are broken too.
    fn deq(&mut self) -> Option<usize> {
        for depth in 0..self.queue.len() {
            let gs = &mut self.queue[depth];
            for i in (0..gs.len()).rev() {
                if self.groups[gs[i]].pending > 0 {
                    let g = gs.remove(i);
                    self.groups[g].broken = true;
                    return Some(g);
                }
            }
            for g in std::mem::take(gs) {
                self.groups[g].broken = true;
            }
        }
        None
    }

    fn delete(&mut self, g: usize) {
        let depth = self.groups[g].depth;
        if let Some(gs) = self.queue.get_mut(depth) {
            gs.retain(|&x| x != g);
        }
    }

    /// `Text#output` / `Breakable#output`; answers the new output width.
    fn output_item(&mut self, item: Item) -> usize {
        match item {
            Item::Text { text, width } => {
                self.output.push_str(&text);
                self.output_width + width
            }
            Item::Breakable {
                sep,
                width,
                indent,
                group,
            } => {
                self.groups[group].pending -= 1;
                if self.groups[group].broken {
                    self.output.push('\n');
                    self.output.push_str(&" ".repeat(indent));
                    indent
                } else {
                    if self.groups[group].pending == 0 {
                        self.delete(group);
                    }
                    self.output.push_str(&sep);
                    self.output_width + width
                }
            }
        }
    }

    fn break_outmost_groups(&mut self) {
        while self.maxwidth < self.output_width + self.buffer_width {
            let Some(g) = self.deq() else { return };
            while self.groups[g].pending > 0 {
                let Some(item) = self.buffer.pop_front() else {
                    break;
                };
                self.buffer_width -= item.width();
                self.output_width = self.output_item(item);
            }
            while matches!(self.buffer.front(), Some(Item::Text { .. })) {
                let item = self.buffer.pop_front().unwrap();
                self.buffer_width -= item.width();
                self.output_width = self.output_item(item);
            }
        }
    }

    pub fn text(&mut self, s: &str) {
        let width = s.chars().count();
        if self.buffer.is_empty() {
            self.output.push_str(s);
            self.output_width += width;
        } else {
            match self.buffer.back_mut() {
                Some(Item::Text { text, width: w }) => {
                    text.push_str(s);
                    *w += width;
                }
                _ => self.buffer.push_back(Item::Text {
                    text: s.to_string(),
                    width,
                }),
            }
            self.buffer_width += width;
            self.break_outmost_groups();
        }
    }

    pub fn breakable(&mut self, sep: &str) {
        let g = *self.group_stack.last().unwrap();
        if self.groups[g].broken {
            self.flush();
            self.output.push('\n');
            self.output.push_str(&" ".repeat(self.indent));
            self.output_width = self.indent;
            self.buffer_width = 0;
        } else {
            let width = sep.chars().count();
            self.groups[g].pending += 1;
            self.buffer.push_back(Item::Breakable {
                sep: sep.to_string(),
                width,
                indent: self.indent,
                group: g,
            });
            self.buffer_width += width;
            self.break_outmost_groups();
        }
    }

    pub fn flush(&mut self) {
        while let Some(item) = self.buffer.pop_front() {
            self.output_width = self.output_item(item);
        }
        self.buffer_width = 0;
    }

    pub fn into_output(mut self) -> String {
        self.flush();
        self.output
    }
}

/// `PP`: the line breaker plus the inspect-key guard that turns a value
/// reached again inside itself into its `pretty_print_cycle` form.
pub struct Printer {
    q: PrettyPrint,
    visiting: Vec<u32>,
}

impl Printer {
    pub fn new(width: usize) -> Self {
        Printer {
            q: PrettyPrint::new(width),
            visiting: Vec::new(),
        }
    }

    pub fn finish(self) -> String {
        self.q.into_output()
    }

    fn comma_breakable(&mut self) {
        self.q.text(",");
        self.q.breakable(" ");
    }

    /// `PPMethods#pp`.
    pub fn pp(&mut self, v: &Value) -> Result<(), String> {
        let id = match v {
            Value::Obj(id) => Some(*id),
            _ => None,
        };
        if let Some(id) = id {
            if self.visiting.contains(&id) {
                return self.q_group(0, "", "", |p| p.pretty_print_cycle(v));
            }
            self.visiting.push(id);
        }
        let r = self.q_group(0, "", "", |p| p.pretty_print(v));
        if id.is_some() {
            self.visiting.pop();
        }
        r
    }

    /// `q.group(indent, open, close) { ... }` with the block given the Printer.
    fn q_group<F>(&mut self, indent: usize, open: &str, close: &str, f: F) -> Result<(), String>
    where
        F: FnOnce(&mut Self) -> Result<(), String>,
    {
        self.q.text(open);
        let depth = self.q.groups[*self.q.group_stack.last().unwrap()].depth + 1;
        let g = self.q.groups.len();
        self.q.groups.push(Group {
            depth,
            pending: 0,
            broken: false,
        });
        self.q.group_stack.push(g);
        self.q.enq(g);
        self.q.indent += indent;
        let r = f(self);
        self.q.indent -= indent;
        self.q.group_stack.pop();
        if self.q.groups[g].pending == 0 {
            self.q.delete(g);
        }
        r?;
        self.q.text(close);
        Ok(())
    }

    fn leaf(&mut self, v: &Value) -> Result<(), String> {
        let s = inspect_of(v)?;
        self.q.text(&s);
        Ok(())
    }

    /// The `pretty_print` of `v`'s class.
    fn pretty_print(&mut self, v: &Value) -> Result<(), String> {
        if let Some(items) = with_host(|h| h.as_array(v)) {
            return self.q_group(1, "[", "]", |p| {
                for (i, x) in items.iter().enumerate() {
                    if i > 0 {
                        p.comma_breakable();
                    }
                    p.pp(x)?;
                }
                Ok(())
            });
        }
        if let Some(map) = with_host(|h| h.as_hash(v)) {
            let pairs: Vec<(Value, Value)> = with_host(|h| {
                map.iter()
                    .map(|(k, val)| (h.key_value(k), val.clone()))
                    .collect()
            });
            return self.pp_hash(&pairs);
        }
        if let Some(items) = with_host(|h| h.as_set(v)) {
            let name = with_host(|h| h.class_of(v));
            return self.q_group(1, &format!("{name}["), "]", |p| {
                for (i, x) in items.iter().enumerate() {
                    if i > 0 {
                        p.comma_breakable();
                    }
                    p.pp(x)?;
                }
                Ok(())
            });
        }
        if let Some(r) = self.range_bounds(v) {
            return self.pp_range(r);
        }
        if let Some(s) = with_host(|h| h.as_str(v)) {
            return self.pp_string(v, &s);
        }
        if let Some((groups, names, _, _)) = with_host(|h| h.as_matchdata(v)) {
            return self.q_group(1, "#<MatchData", ">", |p| {
                p.q.breakable(" ");
                for (i, g) in groups.iter().enumerate() {
                    if i > 0 {
                        p.q.breakable(" ");
                        // A named group shows its name, the others their index.
                        match names.iter().rev().find(|(_, idx)| *idx == i) {
                            Some((n, _)) => p.q.text(n),
                            None => p.q.text(&i.to_string()),
                        }
                        p.q.text(":");
                    }
                    match g {
                        Some(s) => p.q.text(&crate::host::inspect_string(s)),
                        None => p.q.text("nil"),
                    }
                }
                Ok(())
            });
        }
        if let Some(class) = with_host(|h| h.object_class(v)) {
            if let Some((members, _)) = with_host(|h| h.struct_def(&class)) {
                return self.pp_struct(v, &class, &members);
            }
            // `Object#pretty_print`: an `inspect` that is not Kernel's prints as
            // text; Kernel's is `pp_object`.
            if with_host(|h| h.is_plain_object(&class)) && !defines_own(v, "inspect") {
                return self.pp_object(v, &class);
            }
        }
        self.leaf(v)
    }

    /// The `pretty_print_cycle` of `v`'s class.
    fn pretty_print_cycle(&mut self, v: &Value) -> Result<(), String> {
        if let Some(items) = with_host(|h| h.as_array(v)) {
            self.q.text(if items.is_empty() { "[]" } else { "[...]" });
            return Ok(());
        }
        if let Some(map) = with_host(|h| h.as_hash(v)) {
            self.q.text(if map.is_empty() { "{}" } else { "{...}" });
            return Ok(());
        }
        if let Some(items) = with_host(|h| h.as_set(v)) {
            let name = with_host(|h| h.class_of(v));
            let dots = if items.is_empty() { "" } else { "..." };
            self.q.text(&format!("{name}[{dots}]"));
            return Ok(());
        }
        if let Some(class) = with_host(|h| h.object_class(v)) {
            if with_host(|h| h.struct_def(&class)).is_some() {
                let kind = if with_host(|h| h.is_data_class(&class)) {
                    "data"
                } else {
                    "struct"
                };
                let name = if class.starts_with("Struct:") {
                    ""
                } else {
                    &class
                };
                self.q.text(&format!("#<{kind} {name}:...>"));
                return Ok(());
            }
            if with_host(|h| h.is_plain_object(&class)) {
                let head = self.address_head(v, &class);
                return self.q_group(1, &head, ">", |p| {
                    p.q.breakable(" ");
                    p.q.text("...");
                    Ok(())
                });
            }
        }
        self.leaf(v)
    }

    /// `Kernel#to_s` without its closing `>`: `#<Foo:0x…`.
    fn address_head(&self, v: &Value, class: &str) -> String {
        format!("#<{class}:{}", with_host(|h| h.object_address(v)))
    }

    /// `PPMethods#pp_object`.
    fn pp_object(&mut self, v: &Value, class: &str) -> Result<(), String> {
        let mut names = with_host(|h| h.ivar_names(v));
        names.sort();
        let head = self.address_head(v, class);
        self.q_group(1, &head, ">", |p| {
            for (i, name) in names.iter().enumerate() {
                if i > 0 {
                    p.q.text(",");
                }
                p.q.breakable(" ");
                p.q.text(name);
                p.q.text("=");
                let val = with_host(|h| h.ivar_of(v, name.trim_start_matches('@')));
                p.q_group(1, "", "", |p| {
                    p.q.breakable("");
                    p.pp(&val)
                })?;
            }
            Ok(())
        })
    }

    /// `PPMethods#pp_hash` with the 3.4 `pp_hash_pair`.
    fn pp_hash(&mut self, pairs: &[(Value, Value)]) -> Result<(), String> {
        self.q_group(1, "{", "}", |p| {
            for (i, (k, val)) in pairs.iter().enumerate() {
                if i > 0 {
                    p.comma_breakable();
                }
                p.q_group(0, "", "", |p| {
                    if let Some(sym) = with_host(|h| h.as_symbol(k)) {
                        let inspected = inspect_of(k)?;
                        p.q.text(&format!("{}:", sym_key_text(&sym, &inspected)));
                    } else {
                        p.pp(k)?;
                        p.q.text(" ");
                        p.q.text("=>");
                    }
                    p.q_group(1, "", "", |p| {
                        p.q.breakable(" ");
                        p.pp(val)
                    })
                })?;
            }
            Ok(())
        })
    }

    /// `Struct#pretty_print` / `Data#pretty_print`.
    fn pp_struct(&mut self, v: &Value, class: &str, members: &[String]) -> Result<(), String> {
        let anonymous = class.starts_with("Struct:");
        let head = if with_host(|h| h.is_data_class(class)) {
            if anonymous {
                "#<data".to_string()
            } else {
                format!("#<data {class}")
            }
        } else {
            // `sprintf("#<struct %s", nil)` leaves the trailing space.
            format!("#<struct {}", if anonymous { "" } else { class })
        };
        self.q_group(1, &head, ">", |p| {
            for (i, m) in members.iter().enumerate() {
                if i > 0 {
                    p.q.text(",");
                }
                p.q.breakable(" ");
                p.q.text(m);
                p.q.text("=");
                let val = with_host(|h| h.ivar_of(v, m));
                p.q_group(1, "", "", |p| {
                    p.q.breakable("");
                    p.pp(&val)
                })?;
            }
            Ok(())
        })
    }

    /// The two ends of a Range (nil for an open side) and its exclusivity.
    fn range_bounds(&self, v: &Value) -> Option<(Value, Value, bool)> {
        with_host(|h| {
            if let Some((lo, hi, ex)) = h.as_obj_range(v) {
                return Some((lo, hi, ex));
            }
            if let Some((lo, hi, ex)) = h.as_range(v) {
                let end = |n: i64, open: i64| {
                    if n == open {
                        Value::Undef
                    } else {
                        Value::Int(n)
                    }
                };
                return Some((end(lo, RANGE_BEGINLESS), end(hi, RANGE_ENDLESS), ex));
            }
            if let Some((lo, hi, ex)) = h.as_float_range(v) {
                return Some((Value::Float(lo), Value::Float(hi), ex));
            }
            if let Some((lo, hi, ex)) = h.as_str_range(v) {
                return Some((h.new_string(lo), h.new_string(hi), ex));
            }
            None
        })
    }

    /// `Range#pretty_print`.
    fn pp_range(&mut self, (lo, hi, exclusive): (Value, Value, bool)) -> Result<(), String> {
        let lo_nil = matches!(lo, Value::Undef);
        let hi_nil = matches!(hi, Value::Undef);
        if !lo_nil || hi_nil {
            self.pp(&lo)?;
        }
        self.q.breakable("");
        self.q.text(if exclusive { "..." } else { ".." });
        self.q.breakable("");
        if !hi_nil || lo_nil {
            self.pp(&hi)?;
        }
        Ok(())
    }

    /// `String#pretty_print`: a multi-line String prints one `+`-joined
    /// literal per line.
    fn pp_string(&mut self, v: &Value, s: &str) -> Result<(), String> {
        let lines: Vec<&str> = s.split_inclusive('\n').collect();
        if lines.len() <= 1 {
            return self.leaf(v);
        }
        self.q_group(0, "", "", |p| {
            for (i, line) in lines.iter().enumerate() {
                if i > 0 {
                    p.q.text(" +");
                    p.q.breakable(" ");
                }
                let lv = with_host(|h| h.new_string(line.to_string()));
                p.pp(&lv)?;
            }
            Ok(())
        })
    }
}

/// The text before `:` of a Symbol hash key: the bare name, or the quoted name
/// when `Symbol#inspect` would not read back as a label (`pp_hash_pair`'s
/// `%r[\A:["$@!]|[%&*+\-\/<=>@\]^`|~]\z]`).
fn sym_key_text(name: &str, inspected: &str) -> String {
    let second = inspected.chars().nth(1);
    let last = inspected.chars().last();
    let quote = matches!(second, Some('"' | '$' | '@' | '!'))
        || matches!(
            last,
            Some(
                '%' | '&'
                    | '*'
                    | '+'
                    | '-'
                    | '/'
                    | '<'
                    | '='
                    | '>'
                    | '@'
                    | ']'
                    | '^'
                    | '`'
                    | '|'
                    | '~'
            )
        );
    if quote {
        crate::host::inspect_string(name)
    } else {
        name.to_string()
    }
}

/// `PP.width_for($stdout)`: the terminal's columns when stdout is a terminal,
/// else `$COLUMNS`, else 80 — less one.
pub fn stdout_width() -> usize {
    let tty_cols = crate::host::is_standard_stream("stdout")
        .then(|| {
            let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
            let ok = unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) } == 0;
            (ok && ws.ws_col > 0).then_some(ws.ws_col as usize)
        })
        .flatten();
    tty_cols.unwrap_or_else(env_width) - 1
}

/// `PP.width_for` on an output with no `winsize` (a String buffer).
pub fn env_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|&n| n != 0)
        .unwrap_or(80)
}

/// `PP.pp(obj, out, width)` minus the output: the pretty form plus `"\n"`.
pub fn pretty_inspect(v: &Value, width: usize) -> Result<String, String> {
    let mut p = Printer::new(width);
    p.pp(v)?;
    let mut s = p.finish();
    s.push('\n');
    Ok(s)
}
