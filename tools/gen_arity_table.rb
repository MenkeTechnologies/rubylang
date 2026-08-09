# Regenerates `src/arity_table.rs` — the declarative shapes (arity, parameters)
# of MRI's built-in methods, keyed by the module that DEFINES each one.
#
# A built-in has no written parameter list rubylang can inspect, so `Method#arity`
# / `#owner` / `#parameters` on one can only be answered from a table. The table
# is dumped straight out of the reference interpreter rather than hand-written,
# so it says exactly what MRI says.
#
#   ruby tools/gen_arity_table.rb > src/arity_table.rs && cargo fmt
#
# Rows come from the instance-method surface (including inherited, so the owner
# columns fill in Kernel/Comparable/Enumerable/...) plus the singleton surface of
# the core classes. Default-gem owners (did_you_mean, error_highlight) are
# skipped: they are not core semantics and a stripped install does not have them.

CLASSES = %w[
  BasicObject Object Kernel Comparable Enumerable
  Integer Float Numeric Rational Complex
  String Symbol Array Hash Range
  NilClass TrueClass FalseClass
  Proc Method UnboundMethod Module Class
  Exception StandardError ArgumentError RuntimeError TypeError NameError
  NoMethodError ZeroDivisionError IndexError KeyError StopIteration FrozenError
  IOError SystemExit LocalJumpError RangeError NotImplementedError
  Regexp MatchData Struct Time IO File Dir
  Enumerator Enumerator::Lazy Enumerator::Yielder
  Thread Thread::Mutex Thread::Queue Thread::SizedQueue Thread::ConditionVariable
  Random Data Set Binding Fiber Encoding Math Marshal Process GC ObjectSpace
  Signal FileTest Warning
]

# Owners injected by default gems rather than by the core interpreter.
SKIP_OWNER = /\A(DidYouMean|ErrorHighlight)\b/

def esc(s)
  s.gsub('\\', '\\\\\\\\').gsub('"', '\"')
end

# The parameters list MRI reports for a C function of this arity: `n` required
# for a fixed `n`, and `-(n+1)` means `n` required followed by a rest. Rows whose
# real parameters differ (Ruby-defined built-ins with names/defaults/keywords)
# carry them explicitly instead.
def derived_params(arity)
  arity >= 0 ? [[:req]] * arity : [[:req]] * (-arity - 1) + [[:rest]]
end

def encode_params(ps)
  ps.map { |kind, name| name ? "#{kind}:#{name}" : kind.to_s }.join(",")
end

rows = {}
CLASSES.each do |cn|
  k = begin
    Object.const_get(cn)
  rescue NameError
    warn "gen_arity_table: no such constant #{cn}"
    next
  end
  next unless k.is_a?(Module)
  (k.instance_methods + k.private_instance_methods(false)).each do |m|
    um = begin
      k.instance_method(m)
    rescue NameError, TypeError
      next
    end
    rows[[um.owner.to_s, m.to_s]] ||= [um.arity, um.parameters]
  end
  k.methods.each do |m|
    mo = begin
      k.method(m)
    rescue NameError
      next
    end
    next unless mo.owner.to_s.start_with?("#<Class:")
    rows[[mo.owner.to_s, m.to_s]] ||= [mo.arity, mo.parameters]
  end
end
rows.reject! { |(owner, _), _| owner =~ SKIP_OWNER }

ancestry = {}
CLASSES.each do |cn|
  k = Object.const_get(cn) rescue next
  chain = k.ancestors.map(&:to_s).reject { |a| a =~ SKIP_OWNER }
  ancestry[cn] = chain
end

# Every module name reachable as an owner or in an ancestor chain — a singleton
# lookup skips these (a module has no singleton class in the chain sense).
modules = (CLASSES + ancestry.values.flatten).uniq.select do |cn|
  (Object.const_get(cn) rescue nil).instance_of?(Module)
end

puts <<~HDR
  //! MRI's declared built-in method shapes — the arity, defining module and
  //! parameter list of every method the reference interpreter defines on the core
  //! classes.
  //!
  //! A built-in is native code with no written parameter list, so `Method#arity`,
  //! `#owner` and `#parameters` cannot be derived from it the way a `def` allows;
  //! without this table they can only report "variadic, owner unknown". Every row
  //! is what the reference `ruby` reports for that method.
  //!
  //! GENERATED — do not edit by hand. Regenerate against the reference
  //! interpreter with:
  //!
  //! ```text
  //! ruby tools/gen_arity_table.rb > src/arity_table.rs && cargo fmt
  //! ```

  /// `(owner, method, arity, params)`, sorted by `(owner, method)` for binary
  /// search. An empty `params` means the list is the one implied by the arity
  /// (see [`params_for`]); a non-empty one is `kind[:name]`, comma separated.
  pub static BUILTIN_METHODS: &[(&str, &str, i16, &str)] = &[
HDR

rows.sort_by { |(owner, m), _| [owner, m] }.each do |(owner, m), (arity, ps)|
  enc = ps == derived_params(arity) ? "" : encode_params(ps)
  puts %{    ("#{esc(owner)}", "#{esc(m)}", #{arity}, "#{esc(enc)}"),}
end

puts <<~MID
  ];

  /// The ancestor chain the reference interpreter reports for each built-in
  /// class — the order `#owner` resolution walks. Sorted by class name.
  pub static BUILTIN_ANCESTRY: &[(&str, &[&str])] = &[
MID

ancestry.sort.each do |cn, chain|
  puts %{    ("#{esc(cn)}", &[#{chain.map { |c| %{"#{esc(c)}"} }.join(", ")}]),}
end

puts <<~MID2
  ];

  /// The built-in names that are modules, not classes — they have no `new`, and
  /// their singleton chain skips `Class`. Sorted.
  pub static BUILTIN_MODULES: &[&str] = &[
MID2

modules.sort.each { |m| puts %{    "#{esc(m)}",} }

puts <<~'TAIL'
];

/// The row `(owner, arity, params)` the reference interpreter declares for
/// `method` on `owner`, or `None` when `owner` does not define it. The owner is
/// returned as the table's own `'static` string so callers can keep it.
pub fn lookup(owner: &str, method: &str) -> Option<(&'static str, i16, &'static str)> {
    BUILTIN_METHODS
        .binary_search_by(|(o, m, _, _)| (*o, *m).cmp(&(owner, method)))
        .ok()
        .map(|i| (BUILTIN_METHODS[i].0, BUILTIN_METHODS[i].2, BUILTIN_METHODS[i].3))
}

/// The reference ancestor chain of built-in class `name`, or `None` when it is
/// not a built-in.
pub fn ancestry(name: &str) -> Option<&'static [&'static str]> {
    BUILTIN_ANCESTRY
        .binary_search_by(|(c, _)| c.cmp(&name))
        .ok()
        .map(|i| BUILTIN_ANCESTRY[i].1)
}

/// Whether `name` is a built-in module (as opposed to a class).
pub fn is_module(name: &str) -> bool {
    BUILTIN_MODULES.binary_search(&name).is_ok()
}

/// `Method#parameters` for a table row: the explicit list when the row carries
/// one, otherwise the list MRI reports for a C function of that arity — `n`
/// unnamed required parameters, and for a negative arity `-(n+1)` those `n`
/// followed by an unnamed rest.
pub fn params_for(arity: i16, params: &'static str) -> Vec<(&'static str, Option<String>)> {
    if !params.is_empty() {
        return params
            .split(',')
            .map(|tok| match tok.split_once(':') {
                Some((kind, name)) => (param_kind(kind), Some(name.to_string())),
                None => (param_kind(tok), None),
            })
            .collect();
    }
    let req = if arity >= 0 { arity } else { -arity - 1 };
    let mut out: Vec<(&'static str, Option<String>)> = (0..req).map(|_| ("req", None)).collect();
    if arity < 0 {
        out.push(("rest", None));
    }
    out
}

/// Intern a parameter-kind token from the table back to a `'static` name.
fn param_kind(k: &str) -> &'static str {
    match k {
        "req" => "req",
        "opt" => "opt",
        "rest" => "rest",
        "keyreq" => "keyreq",
        "key" => "key",
        "keyrest" => "keyrest",
        "block" => "block",
        "nokey" => "nokey",
        other => Box::leak(other.to_string().into_boxed_str()),
    }
}
TAIL
